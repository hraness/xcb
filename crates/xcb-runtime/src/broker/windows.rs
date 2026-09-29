//! Windows has no descriptor-relative (`openat`) walk yet, and a path-based
//! walk would let a junction or a swapped directory redirect a tool outside
//! the workspace. The workspace tools only serve provider runs and
//! `xcb context prepare`, so opening a workspace refuses with the WSL2
//! guidance and no other method is reachable.

use super::{Listing, ReadResult, Workspace};
use crate::{
    Error, Result,
    broker::snapshot::{CommandChanges, CommandPublication, CommandSnapshot},
};
use serde_json::Value;
use std::path::Path;
use xcb_core::policy::EffectState;

impl Workspace {
    pub fn open(root: &Path) -> Result<Self> {
        Self::open_with_coordination(root, root)
    }
    pub fn open_with_coordination(_root: &Path, _coordination_root: &Path) -> Result<Self> {
        Err(Error::providers_unsupported())
    }
    pub fn root(&self) -> &Path {
        match self.unavailable {}
    }
    pub fn read(&self, _path: &str) -> Result<ReadResult> {
        match self.unavailable {}
    }
    pub fn write(&self, _path: &str, _text: &str, _expected: Option<&str>) -> Result<String> {
        match self.unavailable {}
    }
    pub fn mkdir(&self, _path: &str, _parents: bool) -> Result<usize> {
        match self.unavailable {}
    }
    pub fn remove(&self, _path: &str, _expected: &str) -> Result<()> {
        match self.unavailable {}
    }
    pub fn rename(&self, _from: &str, _to: &str, _expected: &str) -> Result<String> {
        match self.unavailable {}
    }
    pub fn list(&self, _path: &str) -> Result<Listing> {
        match self.unavailable {}
    }
    pub fn search(&self, _path: &str, _query: &str) -> Result<Value> {
        match self.unavailable {}
    }
    pub fn call(&self, _name: &str, _input: &Value) -> Result<Value> {
        match self.unavailable {}
    }
    pub fn call_observed(&self, _name: &str, _input: &Value) -> (Result<Value>, EffectState) {
        match self.unavailable {}
    }
    pub fn context_documents(
        &self,
        _paths: &[String],
    ) -> Result<Vec<crate::context_recipe::Document>> {
        match self.unavailable {}
    }
    pub fn command_snapshot(&self) -> Result<CommandSnapshot> {
        match self.unavailable {}
    }
    pub fn publish_command_changes(
        &self,
        _snapshot: &CommandSnapshot,
        _changes: CommandChanges,
    ) -> (Result<CommandPublication>, EffectState) {
        match self.unavailable {}
    }
}
