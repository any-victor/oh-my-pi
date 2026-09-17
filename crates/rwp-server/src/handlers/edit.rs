//! Edit primitives: replace, patch, ast-edit.

use std::{collections::HashMap, path::PathBuf};

use axum::{
	Json,
	extract::{Path, State},
	response::{IntoResponse, Response},
};
use pi_ast::{SupportLang, ops as ast_ops};
use regex::{Regex, RegexBuilder};
use similar::{Algorithm, TextDiff, capture_diff_slices, get_diff_ratio};
use uuid::Uuid;

use crate::{
	fs_ops::{
		read_file_cached, resolve_cwd_scoped_path,
		write_through::{WriteRequest, decode_text_lossy, write_through},
	},
	protocol::{
		error::{ApiError, ApiResult, ErrorBody},
		requests::{EditAstRequest, EditPatchRequest, EditReplaceRequest, Hunk},
		responses::{AstEditFileChange, AstEditHunk, AstEditResult, AstFileChange, EditResult},
	},
	state::AppState,
};

const FUZZY_MATCH_THRESHOLD: f32 = 0.6;

#[utoipa::path(
	post,
	path = "/sessions/{id}/edit.replace",
	params(("id" = Uuid, Path)),
	request_body = EditReplaceRequest,
	responses(
		(status = 200, body = EditResult),
		(status = 400, body = ErrorBody, description = "bad request"),
		(status = 404, body = ErrorBody, description = "path or session not found"),
		(status = 409, body = ErrorBody, description = "conflict"),
		(status = 412, body = ErrorBody, description = "etag mismatch"),
		(status = 500, body = ErrorBody, description = "internal server error"),
	),
)]
pub async fn edit_replace(
	State(state): State<AppState>,
	Path(id): Path<Uuid>,
	Json(body): Json<EditReplaceRequest>,
) -> ApiResult<Json<EditResult>> {
	if body.old.is_empty() {
		return Err(ApiError::BadRequest("edit.replace requires non-empty `old`".to_owned()));
	}
	let session = state
		.sessions
		.get(id)
		.ok_or_else(|| ApiError::NotFound(format!("session {id}")))?;
	let path = resolve_cwd_scoped_path(&session, &body.path).await?;
	let current = read_file_cached(&session, &path).await?;
	if let Some(if_match) = body.if_match.as_deref()
		&& !any_tag_matches(if_match, &current.etag)
	{
		return Err(ApiError::EtagMismatch);
	}
	let current_text = decode_text_lossy(&current.bytes).into_owned();
	let next_text = replace_text(
		&current_text,
		&body.old,
		&body.new,
		body.fuzzy,
		body.regex,
		body.regex_flags.as_deref(),
		body.all,
	)?;
	let outcome = write_through(WriteRequest {
		session,
		lsp: Some(state.lsp.clone()),
		path: PathBuf::from(&body.path),
		new_bytes: next_text.into_bytes(),
		if_match: Some(current.etag.to_string()),
		preserve_text_conventions: true,
	})
	.await?;
	Ok(Json(EditResult {
		diff:               outcome.diff,
		first_changed_line: outcome.first_changed_line,
		op:                 outcome.op,
	}))
}

#[utoipa::path(
	post,
	path = "/sessions/{id}/edit.patch",
	params(("id" = Uuid, Path)),
	request_body = EditPatchRequest,
	responses(
		(status = 200, body = EditResult),
		(status = 400, body = ErrorBody, description = "bad request"),
		(status = 404, body = ErrorBody, description = "path or session not found"),
		(status = 412, body = ErrorBody, description = "etag mismatch"),
		(status = 500, body = ErrorBody, description = "internal server error"),
	),
)]
pub async fn edit_patch(
	State(state): State<AppState>,
	Path(id): Path<Uuid>,
	Json(body): Json<EditPatchRequest>,
) -> ApiResult<Json<EditResult>> {
	let session = state
		.sessions
		.get(id)
		.ok_or_else(|| ApiError::NotFound(format!("session {id}")))?;
	let path = resolve_cwd_scoped_path(&session, &body.path).await?;
	let current = read_file_cached(&session, &path).await?;
	if let Some(if_match) = body.if_match.as_deref()
		&& !any_tag_matches(if_match, &current.etag)
	{
		return Err(ApiError::EtagMismatch);
	}
	let current_text = decode_text_lossy(&current.bytes).into_owned();
	let next_text = apply_patch(&current_text, &body.hunks)?;
	let outcome = write_through(WriteRequest {
		session,
		lsp: Some(state.lsp.clone()),
		path: PathBuf::from(&body.path),
		new_bytes: next_text.into_bytes(),
		if_match: Some(current.etag.to_string()),
		preserve_text_conventions: true,
	})
	.await?;
	Ok(Json(EditResult {
		diff:               outcome.diff,
		first_changed_line: outcome.first_changed_line,
		op:                 outcome.op,
	}))
}

#[utoipa::path(
	post,
	path = "/sessions/{id}/edit.ast",
	params(("id" = Uuid, Path)),
	request_body = EditAstRequest,
	responses(
		(status = 200, body = AstEditResult),
		(status = 400, body = ErrorBody, description = "bad request"),
		(status = 404, body = ErrorBody, description = "session not found"),
		(status = 500, body = ErrorBody, description = "internal server error"),
	),
)]
pub async fn edit_ast(
	State(state): State<AppState>,
	Path(id): Path<Uuid>,
	Json(body): Json<EditAstRequest>,
) -> Result<Json<AstEditResult>, Response> {
	let session = state
		.sessions
		.get(id)
		.ok_or_else(|| ApiError::NotFound(format!("session {id}")).into_response())?;
	let cancel = session.cancellation_token.child_token();
	ensure_not_cancelled(&cancel).map_err(IntoResponse::into_response)?;
	let matched_files = ast_ops::collect_matched_files(&session.cwd(), &body.paths)
		.map_err(ApiError::Io)
		.map_err(IntoResponse::into_response)?;
	let files_searched = matched_files.len().try_into().unwrap_or(u32::MAX);

	let explicit_language =
		parse_ast_language(body.language.as_deref()).map_err(IntoResponse::into_response)?;
	let rules = body
		.ops
		.iter()
		.map(|op| (op.pat.clone(), op.out.clone()))
		.collect::<Vec<_>>();
	let mut compiled_ops = HashMap::<SupportLang, Vec<pi_ast::ops::CompiledRewrite>>::new();
	let mut changes = Vec::new();
	let mut file_changes = Vec::new();
	for file in matched_files {
		ensure_not_cancelled(&cancel).map_err(IntoResponse::into_response)?;
		let language = explicit_language.or_else(|| SupportLang::from_path(&file.absolute_path));
		let Some(language) = language else {
			continue;
		};
		let ops = if let Some(existing) = compiled_ops.get(&language).cloned() {
			existing
		} else {
			ensure_not_cancelled(&cancel).map_err(IntoResponse::into_response)?;
			let compiled =
				ast_ops::compile_rewrite_rules(&rules, language).map_err(|(op_index, error)| {
					pattern_error_response(
						"pattern failed to parse",
						&body.ops[op_index].pat,
						Some(op_index),
						language,
						error,
					)
				})?;
			compiled_ops.insert(language, compiled.clone());
			compiled
		};
		ensure_not_cancelled(&cancel).map_err(IntoResponse::into_response)?;
		let source = tokio::fs::read_to_string(&file.absolute_path)
			.await
			.map_err(ApiError::Io)
			.map_err(IntoResponse::into_response)?;
		ensure_not_cancelled(&cancel).map_err(IntoResponse::into_response)?;
		let (rewritten, replacements) = ast_ops::rewrite_source(&source, language, &ops)
			.map_err(|error| ApiError::Internal(anyhow::Error::msg(error)))
			.map_err(IntoResponse::into_response)?;
		if replacements == 0 || rewritten == source {
			continue;
		}
		let diff = TextDiff::from_lines(source.as_str(), rewritten.as_str())
			.unified_diff()
			.header("before", "after")
			.to_string();
		let file_change =
			build_ast_edit_file_change(&file.relative_path, replacements, &source, &rewritten, &diff);
		if body.dry_run {
			changes.push(AstFileChange { path: file.relative_path, replacements, diff });
			file_changes.push(file_change);
			continue;
		}
		ensure_not_cancelled(&cancel).map_err(IntoResponse::into_response)?;
		write_through(WriteRequest {
			session: session.clone(),
			lsp: Some(state.lsp.clone()),
			path: PathBuf::from(&file.relative_path),
			new_bytes: rewritten.into_bytes(),
			if_match: Some("*".to_owned()),
			preserve_text_conventions: true,
		})
		.await
		.map_err(IntoResponse::into_response)?;
		changes.push(AstFileChange { path: file.relative_path, replacements, diff });
		file_changes.push(file_change);
	}
	Ok(Json(AstEditResult {
		changes,
		file_changes,
		files_searched,
		limit_reached: false,
		parse_errors: Vec::new(),
		written: !body.dry_run,
		truncated: false,
		exceeded_limit: false,
	}))
}

fn build_ast_edit_file_change(
	path: &str,
	replacements: u32,
	before: &str,
	after: &str,
	diff: &str,
) -> AstEditFileChange {
	let (before_lines, _) = split_lines(before);
	let (after_lines, _) = split_lines(after);
	AstEditFileChange {
		path: path.to_owned(),
		replacements: usize::try_from(replacements).unwrap_or(usize::MAX),
		before_lines,
		after_lines,
		hunks: parse_ast_edit_hunks(diff),
	}
}

fn parse_ast_edit_hunks(diff: &str) -> Vec<AstEditHunk> {
	let mut hunks = Vec::new();
	let mut current_before_start = None;
	let mut current_before_line = 0_u32;
	let mut pending_before_start = None;
	let mut pending_before_lines = Vec::new();
	let mut pending_after_lines = Vec::new();

	let flush_pending = |hunks: &mut Vec<AstEditHunk>,
	                     pending_before_start: &mut Option<u32>,
	                     pending_before_lines: &mut Vec<String>,
	                     pending_after_lines: &mut Vec<String>| {
		if let Some(before_start) = pending_before_start.take() {
			hunks.push(AstEditHunk {
				before_start,
				before_lines: std::mem::take(pending_before_lines),
				after_lines: std::mem::take(pending_after_lines),
			});
		}
	};

	for line in diff.lines() {
		if let Some(before_start) = parse_unified_diff_before_start(line) {
			flush_pending(
				&mut hunks,
				&mut pending_before_start,
				&mut pending_before_lines,
				&mut pending_after_lines,
			);
			current_before_start = Some(before_start);
			current_before_line = before_start;
			continue;
		}
		let Some(_) = current_before_start else {
			continue;
		};
		if line.starts_with("--- ") || line.starts_with("+++ ") || line.starts_with("\\ No newline") {
			continue;
		}
		match line.as_bytes().first().copied() {
			Some(b' ') => {
				flush_pending(
					&mut hunks,
					&mut pending_before_start,
					&mut pending_before_lines,
					&mut pending_after_lines,
				);
				current_before_line = current_before_line.saturating_add(1);
			},
			Some(b'-') => {
				if pending_before_start.is_none() {
					pending_before_start = Some(current_before_line);
				}
				pending_before_lines.push(line[1..].to_owned());
				current_before_line = current_before_line.saturating_add(1);
			},
			Some(b'+') => {
				if pending_before_start.is_none() {
					pending_before_start = Some(current_before_line);
				}
				pending_after_lines.push(line[1..].to_owned());
			},
			_ => {},
		}
	}
	flush_pending(
		&mut hunks,
		&mut pending_before_start,
		&mut pending_before_lines,
		&mut pending_after_lines,
	);
	hunks
}

fn parse_unified_diff_before_start(line: &str) -> Option<u32> {
	parse_unified_diff_range_start(line, "-")
}

fn parse_unified_diff_range_start(line: &str, prefix: &str) -> Option<u32> {
	if !line.starts_with("@@ ") {
		return None;
	}
	let marker = if prefix == "-" { " -" } else { " +" };
	let start = line.find(marker)? + marker.len();
	let rest = &line[start..];
	let end = rest.find([',', ' ']).unwrap_or(rest.len());
	rest[..end].parse().ok()
}

fn any_tag_matches(raw: &str, etag: &str) -> bool {
	raw.split(',').any(|candidate| {
		let trimmed = candidate.trim();
		let strong = trimmed.strip_prefix("W/").unwrap_or(trimmed);
		strong == "*" || strong.trim_matches('"') == etag
	})
}

fn replace_text(
	current: &str,
	old: &str,
	new: &str,
	fuzzy: bool,
	regex: bool,
	regex_flags: Option<&str>,
	all: bool,
) -> ApiResult<String> {
	if regex {
		return replace_regex(current, old, new, regex_flags, all);
	}
	let matches = current
		.match_indices(old)
		.map(|(index, _)| index)
		.collect::<Vec<_>>();
	match matches.as_slice() {
		[index] => {
			let end = index + old.len();
			let mut updated = String::with_capacity(current.len() - old.len() + new.len());
			updated.push_str(&current[..*index]);
			updated.push_str(new);
			updated.push_str(&current[end..]);
			Ok(updated)
		},
		[] if fuzzy => fuzzy_replace(current, old, new),
		[] => Err(ApiError::EtagMismatch),
		_ if all => Ok(current.replace(old, new)),
		_ => Err(ApiError::Conflict(
			"edit.replace matched multiple regions; refine `old` or use edit.patch".to_owned(),
		)),
	}
}

fn replace_regex(
	current: &str,
	old: &str,
	new: &str,
	regex_flags: Option<&str>,
	all: bool,
) -> ApiResult<String> {
	let (regex, global) = build_regex(old, regex_flags, all)?;
	if global {
		Ok(regex.replace_all(current, new).into_owned())
	} else {
		Ok(regex.replacen(current, 1, new).into_owned())
	}
}

fn build_regex(pattern: &str, regex_flags: Option<&str>, all: bool) -> ApiResult<(Regex, bool)> {
	let mut builder = RegexBuilder::new(pattern);
	let mut global = regex_flags.is_none() && all;
	let mut seen = 0_u16;
	for flag in regex_flags.unwrap_or_default().chars() {
		let bit = match flag {
			'g' => 1,
			'i' => 2,
			'm' => 4,
			's' => 8,
			'u' => 16,
			_ => {
				return Err(ApiError::BadRequest(format!(
					"unsupported regex flag `{flag}` for edit.replace"
				)));
			},
		};
		if seen & bit != 0 {
			return Err(ApiError::BadRequest(format!("duplicate regex flag `{flag}`")));
		}
		seen |= bit;
		match flag {
			'g' => global = true,
			'i' => {
				builder.case_insensitive(true);
			},
			'm' => {
				builder.multi_line(true);
			},
			's' => {
				builder.dot_matches_new_line(true);
			},
			'u' => {},
			_ => unreachable!("validated above"),
		}
	}
	let regex = builder
		.build()
		.map_err(|error| ApiError::BadRequest(format!("invalid edit.replace regex: {error}")))?;
	Ok((regex, global))
}

fn fuzzy_replace(current: &str, old: &str, new: &str) -> ApiResult<String> {
	let (current_lines, current_trailing_newline) = split_lines(current);
	let (old_lines, _) = split_lines(old);
	if old_lines.is_empty() {
		return Err(ApiError::BadRequest(
			"fuzzy replace requires at least one logical line in `old`".to_owned(),
		));
	}
	if current_lines.len() < old_lines.len() {
		return Err(ApiError::EtagMismatch);
	}
	let mut best_index = None;
	let mut best_ratio = 0.0_f32;
	let mut ambiguous = false;
	for start in 0..=current_lines.len() - old_lines.len() {
		let window = &current_lines[start..start + old_lines.len()];
		let ops = capture_diff_slices(Algorithm::Patience, &old_lines, window);
		let ratio = get_diff_ratio(&ops, old_lines.len(), window.len());
		if ratio > best_ratio {
			best_ratio = ratio;
			best_index = Some(start);
			ambiguous = false;
		} else if (ratio - best_ratio).abs() < f32::EPSILON && ratio >= FUZZY_MATCH_THRESHOLD {
			ambiguous = true;
		}
	}
	if ambiguous {
		return Err(ApiError::Conflict(
			"fuzzy replace matched multiple regions; refine `old` or use edit.patch".to_owned(),
		));
	}
	let Some(start) = best_index.filter(|_| best_ratio >= FUZZY_MATCH_THRESHOLD) else {
		return Err(ApiError::EtagMismatch);
	};
	let (replacement_lines, _) = split_lines(new);
	let mut updated = Vec::with_capacity(
		current_lines
			.len()
			.saturating_sub(old_lines.len())
			.saturating_add(replacement_lines.len()),
	);
	updated.extend_from_slice(&current_lines[..start]);
	updated.extend(replacement_lines);
	updated.extend_from_slice(&current_lines[start + old_lines.len()..]);
	Ok(render_lines(&updated, current_trailing_newline))
}

fn apply_patch(current: &str, hunks: &[Hunk]) -> ApiResult<String> {
	let (mut lines, trailing_newline) = split_lines(current);
	let original_len = lines.len();
	let mut prior_end = 0_usize;
	let mut line_delta = 0_isize;
	for hunk in hunks {
		if hunk.start == 0 {
			return Err(ApiError::BadRequest(
				"patch hunks are 1-based; start=0 is invalid".to_owned(),
			));
		}
		let start = usize::try_from(hunk.start - 1)
			.map_err(|_| ApiError::BadRequest("patch hunk start overflowed usize".to_owned()))?;
		let deleted = usize::try_from(hunk.deleted).map_err(|_| {
			ApiError::BadRequest("patch hunk delete count overflowed usize".to_owned())
		})?;
		if start < prior_end {
			return Err(ApiError::BadRequest("patch hunks overlap or are out of order".to_owned()));
		}
		if start > original_len || start.saturating_add(deleted) > original_len {
			return Err(ApiError::EtagMismatch);
		}
		let actual_start = start
			.checked_add_signed(line_delta)
			.ok_or(ApiError::EtagMismatch)?;
		let actual_end = actual_start
			.checked_add(deleted)
			.ok_or(ApiError::EtagMismatch)?;
		if actual_end > lines.len() {
			return Err(ApiError::EtagMismatch);
		}
		lines.splice(actual_start..actual_end, hunk.inserted.iter().cloned());
		line_delta += isize::try_from(hunk.inserted.len())
			.map_err(|_| ApiError::BadRequest("patch insertion overflowed isize".to_owned()))?
			- isize::try_from(deleted)
				.map_err(|_| ApiError::BadRequest("patch delete count overflowed isize".to_owned()))?;
		prior_end = start.saturating_add(deleted);
	}
	Ok(render_lines(&lines, trailing_newline))
}

fn split_lines(text: &str) -> (Vec<String>, bool) {
	let mut lines = Vec::new();
	let mut start = 0_usize;
	let mut index = 0_usize;
	let bytes = text.as_bytes();
	while index < bytes.len() {
		match bytes[index] {
			b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
				lines.push(text[start..index].to_owned());
				index += 2;
				start = index;
			},
			b'\n' | b'\r' => {
				lines.push(text[start..index].to_owned());
				index += 1;
				start = index;
			},
			_ => index += 1,
		}
	}
	let trailing_newline = matches!(bytes.last(), Some(b'\n' | b'\r'));
	if start < text.len() {
		lines.push(text[start..].to_owned());
	}
	(lines, trailing_newline)
}

fn render_lines(lines: &[String], trailing_newline: bool) -> String {
	if lines.is_empty() {
		return String::new();
	}
	let body_len = lines.iter().map(String::len).sum::<usize>();
	let separators = lines.len().saturating_sub(1);
	let mut rendered = String::with_capacity(body_len + separators + usize::from(trailing_newline));
	for (index, line) in lines.iter().enumerate() {
		if index > 0 {
			rendered.push('\n');
		}
		rendered.push_str(line);
	}
	if trailing_newline {
		rendered.push('\n');
	}
	rendered
}
fn parse_ast_language(raw: Option<&str>) -> ApiResult<Option<SupportLang>> {
	raw.map(|language| {
		SupportLang::from_alias(language)
			.ok_or_else(|| ApiError::BadRequest(format!("unsupported ast language `{language}`")))
	})
	.transpose()
}

fn ensure_not_cancelled(cancel: &tokio_util::sync::CancellationToken) -> ApiResult<()> {
	if cancel.is_cancelled() {
		return Err(ApiError::Cancelled);
	}
	Ok(())
}

fn pattern_error_response(
	message: &str,
	pattern: &str,
	op_index: Option<usize>,
	language: SupportLang,
	error: impl std::fmt::Display,
) -> Response {
	(
		axum::http::StatusCode::BAD_REQUEST,
		Json(ErrorBody {
			code:    "bad-request".to_owned(),
			message: message.to_owned(),
			detail:  Some(serde_json::json!({
				"pattern": pattern,
				"op_index": op_index,
				"language": language.canonical_name(),
				"reason": error.to_string(),
			})),
		}),
	)
		.into_response()
}

#[cfg(test)]
mod tests {
	use tokio_util::sync::CancellationToken;

	use super::ensure_not_cancelled;

	#[test]
	fn ensure_not_cancelled_allows_live_requests() {
		let cancel = CancellationToken::new();
		assert!(ensure_not_cancelled(&cancel).is_ok());
	}

	#[test]
	fn ensure_not_cancelled_maps_cancel_to_499() {
		let cancel = CancellationToken::new();
		cancel.cancel();
		let error = ensure_not_cancelled(&cancel).expect_err("cancelled response");
		assert_eq!(error.status().as_u16(), 499);
	}
}
