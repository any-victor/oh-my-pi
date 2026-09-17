//! `SessionRegistry`: concurrent `Uuid -> Session` map.

use std::sync::Arc;

use dashmap::DashMap;
use uuid::Uuid;

use super::core::Session;

#[derive(Debug, Default)]
pub struct SessionRegistry {
	map: DashMap<Uuid, Arc<Session>>,
}

impl SessionRegistry {
	#[must_use]
	pub fn new() -> Self {
		Self::default()
	}

	pub fn insert(&self, session: Arc<Session>) -> Uuid {
		let id = session.id;
		self.map.insert(id, session);
		id
	}

	pub fn get(&self, id: Uuid) -> Option<Arc<Session>> {
		self.map.get(&id).map(|r| r.clone())
	}

	pub fn remove(&self, id: Uuid) -> Option<Arc<Session>> {
		self.map.remove(&id).map(|(_, s)| s)
	}

	pub fn len(&self) -> usize {
		self.map.len()
	}

	pub fn is_empty(&self) -> bool {
		self.map.is_empty()
	}
}
