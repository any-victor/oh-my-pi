//! Workspace startup scan, ported off `pi-natives` onto `pi-walker` + `ignore`.

use std::{
	collections::HashSet,
	path::{Path, PathBuf},
	sync::{
		Arc, LazyLock,
		atomic::{AtomicBool, Ordering},
	},
	time::{Duration, Instant},
};

use ignore::{DirEntry, ParallelVisitor, ParallelVisitorBuilder, WalkBuilder, WalkState};
use parking_lot::Mutex;
use pi_walker::{FileType, classify_file_type, normalize_relative_path, resolve_search_path};

const AGENTS_MD_FILENAME: &str = "AGENTS.md";
const AGENTS_MD_MIN_DEPTH: usize = 1;
const AGENTS_MD_MAX_DEPTH: usize = 4;
const AGENTS_MD_LIMIT: usize = 200;
const MAX_ENTRIES: usize = 100_000;

const EXCLUDED_DIRS: &[&str] = &[
	"node_modules",
	".git",
	".next",
	"dist",
	"build",
	"target",
	".venv",
	".cache",
	".turbo",
	".parcel-cache",
	"coverage",
];

static EXCLUDED_DIR_SET: LazyLock<HashSet<&'static str>> =
	LazyLock::new(|| EXCLUDED_DIRS.iter().copied().collect());

/// Input options for the single-pass workspace startup scan.
pub struct ListWorkspaceOptions {
	pub path:              String,
	pub max_depth:         u32,
	pub hidden:            Option<bool>,
	pub gitignore:         Option<bool>,
	pub collect_agents_md: Option<bool>,
	pub timeout_ms:        Option<u32>,
}

/// One tree entry from a workspace scan.
#[derive(Clone, Debug)]
pub struct WorkspaceMatch {
	pub path:      String,
	pub file_type: FileType,
	pub mtime:     Option<f64>,
	pub size:      Option<f64>,
}

/// Result payload returned by a workspace scan.
pub struct ListWorkspaceResult {
	pub entries:         Vec<WorkspaceMatch>,
	pub agents_md_files: Vec<String>,
	pub truncated:       bool,
}

struct WorkspaceConfig {
	root:              PathBuf,
	max_depth:         usize,
	walk_max_depth:    usize,
	include_hidden:    bool,
	use_gitignore:     bool,
	collect_agents_md: bool,
}

fn build_workspace_walker(config: &WorkspaceConfig) -> WalkBuilder {
	let mut builder = WalkBuilder::new(&config.root);
	builder
		.hidden(!config.include_hidden)
		.follow_links(false)
		.sort_by_file_path(|a, b| a.cmp(b))
		.max_depth(Some(config.walk_max_depth))
		.filter_entry(|entry| {
			let name = entry.file_name().to_str().unwrap_or_default();
			if name == ".DS_Store" {
				return false;
			}
			if entry
				.file_type()
				.is_some_and(|file_type| file_type.is_dir())
				&& EXCLUDED_DIR_SET.contains(name)
			{
				return false;
			}
			true
		});

	if config.use_gitignore {
		builder
			.git_ignore(true)
			.git_exclude(true)
			.git_global(true)
			.ignore(true)
			.parents(true)
			.require_git(false);
	} else {
		builder
			.git_ignore(false)
			.git_exclude(false)
			.git_global(false)
			.ignore(false)
			.parents(false);
	}

	builder
}

fn glob_match_from_path(root: &Path, path: &Path) -> Option<WorkspaceMatch> {
	let relative = normalize_relative_path(root, path);
	if relative.is_empty() {
		return None;
	}
	let (file_type, mtime, size) = classify_file_type(path)?;
	Some(WorkspaceMatch {
		path: relative.into_owned(),
		file_type,
		mtime,
		size: size.map(|value| value as f64),
	})
}

fn glob_match_from_entry(root: &Path, entry: &DirEntry) -> Option<WorkspaceMatch> {
	glob_match_from_path(root, entry.path())
}

fn is_file_or_file_symlink(path: &Path, file_type: FileType) -> bool {
	match file_type {
		FileType::File => true,
		FileType::Symlink => std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file()),
		FileType::Dir => false,
	}
}

fn collect_agents_md_in_directory(
	config: &WorkspaceConfig,
	directory: &Path,
	directory_depth: usize,
	entries: &mut Vec<WorkspaceMatch>,
	agents_md_files: &mut Vec<String>,
) {
	if !config.collect_agents_md {
		return;
	}
	let candidate = directory.join(AGENTS_MD_FILENAME);
	let Some(entry) = glob_match_from_path(&config.root, &candidate) else {
		return;
	};
	if !is_file_or_file_symlink(&candidate, entry.file_type) {
		return;
	}
	let tree_depth = directory_depth + 1;
	if tree_depth <= config.max_depth {
		entries.push(entry.clone());
	}
	if (AGENTS_MD_MIN_DEPTH..=AGENTS_MD_MAX_DEPTH).contains(&directory_depth) {
		agents_md_files.push(entry.path);
	}
}

struct WorkspaceVisitor<'a> {
	config:                 &'a WorkspaceConfig,
	deadline:               Option<Instant>,
	timed_out:              &'a AtomicBool,
	entries:                Vec<WorkspaceMatch>,
	agents_md_files:        Vec<String>,
	shared_entries:         Arc<Mutex<Vec<Vec<WorkspaceMatch>>>>,
	shared_agents_md_files: Arc<Mutex<Vec<Vec<String>>>>,
	visited:                usize,
}

impl Drop for WorkspaceVisitor<'_> {
	fn drop(&mut self) {
		if !self.entries.is_empty() {
			let entries = std::mem::take(&mut self.entries);
			self.shared_entries.lock().push(entries);
		}
		if !self.agents_md_files.is_empty() {
			let agents_md_files = std::mem::take(&mut self.agents_md_files);
			self.shared_agents_md_files.lock().push(agents_md_files);
		}
	}
}

impl ParallelVisitor for WorkspaceVisitor<'_> {
	fn visit(&mut self, entry: std::result::Result<DirEntry, ignore::Error>) -> WalkState {
		if self.visited == 0 || self.visited >= 128 {
			self.visited = 0;
			if self.deadline.is_some_and(|deadline| Instant::now() >= deadline) {
				self.timed_out.store(true, Ordering::Relaxed);
				return WalkState::Quit;
			}
		}
		self.visited += 1;

		let Ok(entry) = entry else {
			return WalkState::Continue;
		};
		let entry_depth = entry.depth();
		if entry
			.file_type()
			.is_some_and(|file_type| file_type.is_dir())
		{
			collect_agents_md_in_directory(
				self.config,
				entry.path(),
				entry_depth,
				&mut self.entries,
				&mut self.agents_md_files,
			);
		}
		if entry_depth <= self.config.max_depth
			&& let Some(entry) = glob_match_from_entry(&self.config.root, &entry)
		{
			self.entries.push(entry);
		}
		WalkState::Continue
	}
}

struct WorkspaceVisitorBuilder<'a> {
	config:                 &'a WorkspaceConfig,
	deadline:               Option<Instant>,
	timed_out:              &'a AtomicBool,
	shared_entries:         Arc<Mutex<Vec<Vec<WorkspaceMatch>>>>,
	shared_agents_md_files: Arc<Mutex<Vec<Vec<String>>>>,
}

impl<'a> ParallelVisitorBuilder<'a> for WorkspaceVisitorBuilder<'a> {
	fn build(&mut self) -> Box<dyn ParallelVisitor + 'a> {
		Box::new(WorkspaceVisitor {
			config:                 self.config,
			deadline:               self.deadline,
			timed_out:              self.timed_out,
			entries:                Vec::new(),
			agents_md_files:        Vec::new(),
			shared_entries:         Arc::clone(&self.shared_entries),
			shared_agents_md_files: Arc::clone(&self.shared_agents_md_files),
			visited:                0,
		})
	}
}

fn sort_dedup_entries(entries: &mut Vec<WorkspaceMatch>) {
	entries.sort_unstable_by(|a, b| a.path.cmp(&b.path));
	entries.dedup_by(|a, b| a.path == b.path);
}

fn sort_dedup_paths(paths: &mut Vec<String>) {
	paths.sort_unstable();
	paths.dedup();
}

fn run_list_workspace(
	config: WorkspaceConfig,
	timeout_ms: Option<u32>,
) -> Result<ListWorkspaceResult, anyhow::Error> {
	let deadline = timeout_ms.map(|ms| Instant::now() + Duration::from_millis(u64::from(ms)));
	let timed_out = AtomicBool::new(false);
	let mut root_entries = Vec::new();
	let mut root_agents_md_files = Vec::new();
	collect_agents_md_in_directory(
		&config,
		&config.root,
		0,
		&mut root_entries,
		&mut root_agents_md_files,
	);

	let mut builder = build_workspace_walker(&config);
	let workers = pi_walker::walk_workers();
	if workers > 0 {
		builder.threads(workers);
	}

	let shared_entries = Arc::new(Mutex::new(Vec::new()));
	let shared_agents_md_files = Arc::new(Mutex::new(Vec::new()));
	let mut visitor_builder = WorkspaceVisitorBuilder {
		config:                 &config,
		deadline,
		timed_out:              &timed_out,
		shared_entries:         Arc::clone(&shared_entries),
		shared_agents_md_files: Arc::clone(&shared_agents_md_files),
	};

	builder.build_parallel().visit(&mut visitor_builder);
	if timed_out.load(Ordering::Relaxed) {
		anyhow::bail!("workspace scan timed out");
	}

	let mut entries: Vec<WorkspaceMatch> = shared_entries.lock().drain(..).flatten().collect();
	entries.extend(root_entries);
	sort_dedup_entries(&mut entries);

	let mut agents_md_files: Vec<String> =
		shared_agents_md_files.lock().drain(..).flatten().collect();
	agents_md_files.extend(root_agents_md_files);
	sort_dedup_paths(&mut agents_md_files);

	let entries_truncated = entries.len() > MAX_ENTRIES;
	if entries_truncated {
		entries.truncate(MAX_ENTRIES);
	}
	let agents_md_truncated = agents_md_files.len() > AGENTS_MD_LIMIT;
	if agents_md_truncated {
		agents_md_files.truncate(AGENTS_MD_LIMIT);
	}

	Ok(ListWorkspaceResult {
		entries,
		agents_md_files,
		truncated: entries_truncated || agents_md_truncated,
	})
}

/// Walk the workspace once and return tree entries plus AGENTS.md candidates.
pub fn list_workspace_blocking(
	options: ListWorkspaceOptions,
) -> Result<ListWorkspaceResult, anyhow::Error> {
	let max_depth = options.max_depth as usize;
	let collect_agents_md = options.collect_agents_md.unwrap_or(false);
	let walk_max_depth = if collect_agents_md {
		max_depth.max(AGENTS_MD_MAX_DEPTH)
	} else {
		max_depth
	};
	run_list_workspace(
		WorkspaceConfig {
			root: resolve_search_path(&options.path).map_err(|error| anyhow::anyhow!("{error}"))?,
			max_depth,
			walk_max_depth,
			include_hidden: options.hidden.unwrap_or(false),
			use_gitignore: options.gitignore.unwrap_or(true),
			collect_agents_md,
		},
		options.timeout_ms,
	)
}
