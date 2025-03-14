// Copyright (C) 2024 Quickwit, Inc.
//
// Quickwit is offered under the AGPL v3.0 and as commercial software.
// For commercial licensing, contact us at hello@quickwit.io.
//
// AGPL:
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program. If not, see <http://www.gnu.org/licenses/>.

use std::collections::hash_map::Entry;
use std::collections::BTreeSet;
use std::ops::{Deref, DerefMut};
use std::time::Duration;

use fnv::{FnvHashMap, FnvHashSet};
use quickwit_common::rate_limiter::{RateLimiter, RateLimiterSettings};
use quickwit_common::tower::ConstantRate;
use quickwit_ingest::{RateMibPerSec, ShardInfo, ShardInfos};
use quickwit_proto::ingest::{Shard, ShardState};
use quickwit_proto::types::{IndexUid, NodeId, ShardId, SourceId, SourceUid};
use tracing::{error, warn};

/// Limits the number of shards that can be opened for scaling up a source to 5 per minute.
const SCALING_UP_RATE_LIMITER_SETTINGS: RateLimiterSettings = RateLimiterSettings {
    burst_limit: 5,
    rate_limit: ConstantRate::new(5, Duration::from_secs(60)),
    refill_period: Duration::from_secs(12),
};

/// Limits the number of shards that can be closed for scaling down a source to 1 per minute.
const SCALING_DOWN_RATE_LIMITER_SETTINGS: RateLimiterSettings = RateLimiterSettings {
    burst_limit: 1,
    rate_limit: ConstantRate::new(1, Duration::from_secs(60)),
    refill_period: Duration::from_secs(60),
};

#[derive(Debug, Clone, Copy)]
pub(crate) enum ScalingMode {
    Up,
    Down,
}

#[derive(Debug, Clone)]
pub(crate) struct ShardEntry {
    pub shard: Shard,
    pub ingestion_rate: RateMibPerSec,
}

impl Deref for ShardEntry {
    type Target = Shard;

    fn deref(&self) -> &Self::Target {
        &self.shard
    }
}

impl DerefMut for ShardEntry {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.shard
    }
}

impl From<Shard> for ShardEntry {
    fn from(shard: Shard) -> Self {
        Self {
            shard,
            ingestion_rate: RateMibPerSec::default(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct ShardTableEntry {
    shard_entries: FnvHashMap<ShardId, ShardEntry>,
    scaling_up_rate_limiter: RateLimiter,
    scaling_down_rate_limiter: RateLimiter,
}

impl Default for ShardTableEntry {
    fn default() -> Self {
        Self {
            shard_entries: Default::default(),
            scaling_up_rate_limiter: RateLimiter::from_settings(SCALING_UP_RATE_LIMITER_SETTINGS),
            scaling_down_rate_limiter: RateLimiter::from_settings(
                SCALING_DOWN_RATE_LIMITER_SETTINGS,
            ),
        }
    }
}

impl ShardTableEntry {
    pub fn from_shards(shards: Vec<Shard>) -> Self {
        let shard_entries = shards
            .into_iter()
            .filter(|shard| {
                let shard_state = shard.shard_state();
                shard_state == ShardState::Open || shard_state == ShardState::Closed
            })
            .map(|shard| (shard.shard_id().clone(), shard.into()))
            .collect();
        Self {
            shard_entries,
            ..Default::default()
        }
    }

    fn is_empty(&self) -> bool {
        self.shard_entries.is_empty()
    }
}

// A table that keeps track of the existing shards for each index and source,
// and for each ingester, the list of shards it is supposed to host.
//
// (All mutable methods must maintain the two consistent)
#[derive(Debug, Default)]
pub(crate) struct ShardTable {
    table_entries: FnvHashMap<SourceUid, ShardTableEntry>,
    ingester_shards: FnvHashMap<NodeId, FnvHashMap<SourceUid, BTreeSet<ShardId>>>,
}

// Removes the shards from the ingester_shards map.
//
// This function is used to maintain the shard table invariant.
fn remove_shard_from_ingesters_internal(
    source_uid: &SourceUid,
    shard: &Shard,
    ingester_shards: &mut FnvHashMap<NodeId, FnvHashMap<SourceUid, BTreeSet<ShardId>>>,
) {
    for node in shard.ingester_nodes() {
        let ingester_shards = ingester_shards
            .get_mut(&node)
            .expect("shard table reached inconsistent state");
        let shard_ids = ingester_shards.get_mut(source_uid).unwrap();
        shard_ids.remove(shard.shard_id());
    }
}

impl ShardTable {
    /// Removes all the entries that match the target index ID.
    pub fn delete_index(&mut self, index_id: &str) {
        let shards_removed = self
            .table_entries
            .iter()
            .filter(|(source_uid, _)| source_uid.index_uid.index_id() == index_id)
            .flat_map(|(source_uid, shard_table_entry)| {
                shard_table_entry
                    .shard_entries
                    .values()
                    .map(move |shard_entry: &ShardEntry| (source_uid, &shard_entry.shard))
            });
        for (source_uid, shard) in shards_removed {
            remove_shard_from_ingesters_internal(source_uid, shard, &mut self.ingester_shards);
        }
        self.table_entries
            .retain(|source_uid, _| source_uid.index_uid.index_id() != index_id);
        self.check_invariant();
    }

    /// Checks whether the shard table is consistent.
    ///
    /// Panics if it is not.
    #[allow(clippy::mutable_key_type)]
    fn check_invariant(&self) {
        // This function is expensive! Let's not call it in release mode.
        if !cfg!(debug_assertions) {
            return;
        };
        let mut shard_sets_in_shard_table = FnvHashSet::default();
        for (source_uid, shard_table_entry) in &self.table_entries {
            for (shard_id, shard_entry) in &shard_table_entry.shard_entries {
                debug_assert_eq!(shard_id, shard_entry.shard.shard_id());
                debug_assert_eq!(source_uid.index_uid.as_str(), &shard_entry.shard.index_uid);
                for node in shard_entry.shard.ingester_nodes() {
                    shard_sets_in_shard_table.insert((node, source_uid, shard_id));
                }
            }
        }
        for (node, ingester_shards) in &self.ingester_shards {
            for (source_uid, shard_ids) in ingester_shards {
                for shard_id in shard_ids {
                    let shard_table_entry = self.table_entries.get(source_uid).unwrap();
                    debug_assert!(shard_table_entry.shard_entries.contains_key(shard_id));
                    debug_assert!(shard_sets_in_shard_table.remove(&(
                        node.clone(),
                        source_uid,
                        shard_id
                    )));
                }
            }
        }
    }

    /// Lists all the shards hosted on a given node, regardless of whether it is a
    /// leader or a follower.
    pub fn list_shards_for_node(
        &self,
        ingester: &NodeId,
    ) -> Option<&FnvHashMap<SourceUid, BTreeSet<ShardId>>> {
        self.ingester_shards.get(ingester)
    }

    pub fn list_shards_for_index<'a>(
        &'a self,
        index_uid: &'a IndexUid,
    ) -> impl Iterator<Item = &'a ShardEntry> + 'a {
        self.table_entries
            .iter()
            .filter(move |(source_uid, _)| source_uid.index_uid == *index_uid)
            .flat_map(|(_, shard_table_entry)| shard_table_entry.shard_entries.values())
    }

    pub fn num_shards(&self) -> usize {
        self.table_entries
            .values()
            .map(|shard_table_entry| shard_table_entry.shard_entries.len())
            .sum()
    }

    /// Adds a new empty entry for the given index and source.
    ///
    /// TODO check and document the behavior on error (if the source was already here).
    pub fn add_source(&mut self, index_uid: &IndexUid, source_id: &SourceId) {
        let source_uid = SourceUid {
            index_uid: index_uid.clone(),
            source_id: source_id.clone(),
        };
        let table_entry = ShardTableEntry::default();
        let previous_table_entry_opt = self.table_entries.insert(source_uid, table_entry);
        if let Some(previous_table_entry) = previous_table_entry_opt {
            if !previous_table_entry.is_empty() {
                error!(
                    "shard table entry for index `{}` and source `{}` already exists",
                    index_uid.index_id(),
                    source_id
                );
            }
        }
        self.check_invariant();
    }

    pub fn delete_source(&mut self, index_uid: &IndexUid, source_id: &SourceId) {
        let source_uid = SourceUid {
            index_uid: index_uid.clone(),
            source_id: source_id.clone(),
        };
        let Some(shard_table_entry) = self.table_entries.remove(&source_uid) else {
            return;
        };
        for shard_entry in shard_table_entry.shard_entries.values() {
            remove_shard_from_ingesters_internal(
                &source_uid,
                &shard_entry.shard,
                &mut self.ingester_shards,
            );
        }
        self.check_invariant();
    }

    #[cfg(test)]
    pub(crate) fn all_shards(&self) -> impl Iterator<Item = &ShardEntry> + '_ {
        self.table_entries
            .values()
            .flat_map(|table_entry| table_entry.shard_entries.values())
    }

    pub(crate) fn all_shards_with_source(
        &self,
    ) -> impl Iterator<Item = (&SourceUid, impl Iterator<Item = &ShardEntry>)> + '_ {
        self.table_entries
            .iter()
            .map(|(source, shard_table)| (source, shard_table.shard_entries.values()))
    }

    pub(crate) fn all_shards_mut(&mut self) -> impl Iterator<Item = &mut ShardEntry> + '_ {
        self.table_entries
            .values_mut()
            .flat_map(|table_entry| table_entry.shard_entries.values_mut())
    }

    /// Lists the shards of a given source. Returns `None` if the source does not exist.
    pub fn list_shards(&self, source_uid: &SourceUid) -> Option<impl Iterator<Item = &ShardEntry>> {
        self.table_entries
            .get(source_uid)
            .map(|table_entry| table_entry.shard_entries.values())
    }

    /// Updates the shard table.
    pub fn insert_newly_opened_shards(
        &mut self,
        index_uid: &IndexUid,
        source_id: &SourceId,
        opened_shards: Vec<Shard>,
    ) {
        let source_uid = SourceUid {
            index_uid: index_uid.clone(),
            source_id: source_id.clone(),
        };
        for shard in &opened_shards {
            if shard.index_uid != source_uid.index_uid.as_str()
                || shard.source_id != source_uid.source_id
            {
                panic!(
                    "shard source UID `{}/{}` does not match source UID `{source_uid}`",
                    shard.index_uid, shard.source_id,
                );
            }
        }
        for shard in &opened_shards {
            for node in shard.ingester_nodes() {
                let ingester_shards = self.ingester_shards.entry(node).or_default();
                let shard_ids = ingester_shards.entry(source_uid.clone()).or_default();
                shard_ids.insert(shard.shard_id().clone());
            }
        }
        match self.table_entries.entry(source_uid) {
            Entry::Occupied(mut entry) => {
                let table_entry = entry.get_mut();

                for opened_shard in opened_shards {
                    // We only insert shards that we don't know about because the control plane
                    // knows more about the state of the shards than the metastore.
                    table_entry
                        .shard_entries
                        .entry(opened_shard.shard_id().clone())
                        .or_insert(opened_shard.into());
                }
            }
            // This should never happen if the control plane view is consistent with the state of
            // the metastore, so should we panic here? Warnings are most likely going to go
            // unnoticed.
            Entry::Vacant(entry) => {
                let shard_entries: FnvHashMap<ShardId, ShardEntry> = opened_shards
                    .into_iter()
                    .map(|shard| (shard.shard_id().clone(), shard.into()))
                    .collect();
                let table_entry = ShardTableEntry {
                    shard_entries,
                    ..Default::default()
                };
                entry.insert(table_entry);
            }
        }
        self.check_invariant();
    }

    /// Finds open shards for a given index and source and whose leaders are not in the set of
    /// unavailable ingesters.
    pub fn find_open_shards(
        &self,
        index_uid: &IndexUid,
        source_id: &SourceId,
        unavailable_leaders: &FnvHashSet<NodeId>,
    ) -> Option<Vec<ShardEntry>> {
        let source_uid = SourceUid {
            index_uid: index_uid.clone(),
            source_id: source_id.clone(),
        };
        let table_entry = self.table_entries.get(&source_uid)?;
        let open_shards: Vec<ShardEntry> = table_entry
            .shard_entries
            .values()
            .filter(|shard_entry| {
                shard_entry.shard.is_open() && !unavailable_leaders.contains(&shard_entry.leader_id)
            })
            .cloned()
            .collect();
        Some(open_shards)
    }

    pub fn update_shards(
        &mut self,
        source_uid: &SourceUid,
        shard_infos: &ShardInfos,
    ) -> ShardStats {
        let mut num_open_shards = 0;
        let mut ingestion_rate_sum = RateMibPerSec::default();

        if let Some(table_entry) = self.table_entries.get_mut(source_uid) {
            for shard_info in shard_infos {
                let ShardInfo {
                    shard_id,
                    shard_state,
                    ingestion_rate,
                } = shard_info;

                if let Some(shard_entry) = table_entry.shard_entries.get_mut(shard_id) {
                    shard_entry.ingestion_rate = *ingestion_rate;
                    // `ShardInfos` are broadcasted via Chitchat and eventually consistent. As a
                    // result, we can only trust the `Closed` state, which is final.
                    if shard_state.is_closed() {
                        shard_entry.set_shard_state(ShardState::Closed);
                    }
                }
            }
            for shard_entry in table_entry.shard_entries.values() {
                if shard_entry.is_open() {
                    num_open_shards += 1;
                    ingestion_rate_sum += shard_entry.ingestion_rate;
                }
            }
        }
        let avg_ingestion_rate = if num_open_shards > 0 {
            ingestion_rate_sum.0 as f32 / num_open_shards as f32
        } else {
            0.0
        };

        ShardStats {
            num_open_shards,
            avg_ingestion_rate,
        }
    }

    /// Sets the state of the shards identified by their index UID, source ID, and shard IDs to
    /// `Closed`.
    pub fn close_shards(&mut self, source_uid: &SourceUid, shard_ids: &[ShardId]) -> Vec<ShardId> {
        let mut closed_shard_ids = Vec::new();

        if let Some(table_entry) = self.table_entries.get_mut(source_uid) {
            for shard_id in shard_ids {
                if let Some(shard_entry) = table_entry.shard_entries.get_mut(shard_id) {
                    if !shard_entry.is_closed() {
                        shard_entry.set_shard_state(ShardState::Closed);
                        closed_shard_ids.push(shard_id.clone());
                    }
                }
            }
        }
        closed_shard_ids
    }

    /// Removes the shards identified by their index UID, source ID, and shard IDs.
    pub fn delete_shards(&mut self, source_uid: &SourceUid, shard_ids: &[ShardId]) {
        let mut shard_entries_to_remove: Vec<ShardEntry> = Vec::new();
        if let Some(table_entry) = self.table_entries.get_mut(source_uid) {
            for shard_id in shard_ids {
                if let Some(shard_entry) = table_entry.shard_entries.remove(shard_id) {
                    shard_entries_to_remove.push(shard_entry);
                } else {
                    warn!(shard=%shard_id, "deleting a non-existing shard");
                }
            }
        }
        for shard_entry in shard_entries_to_remove {
            remove_shard_from_ingesters_internal(
                source_uid,
                &shard_entry.shard,
                &mut self.ingester_shards,
            );
        }
        self.check_invariant();
    }

    /// Set the shards for a given source.
    /// This function panics if an entry was previously associated to the source uid.
    pub(crate) fn initialize_source_shards(&mut self, source_uid: SourceUid, shards: Vec<Shard>) {
        for shard in &shards {
            for node in shard.ingester_nodes() {
                let ingester_shards = self.ingester_shards.entry(node).or_default();
                let shard_ids = ingester_shards.entry(source_uid.clone()).or_default();
                shard_ids.insert(shard.shard_id().clone());
            }
        }
        let table_entry = ShardTableEntry::from_shards(shards);
        let previous_entry = self.table_entries.insert(source_uid, table_entry);
        assert!(previous_entry.is_none());
        self.check_invariant();
    }

    pub fn acquire_scaling_permits(
        &mut self,
        source_uid: &SourceUid,
        scaling_mode: ScalingMode,
        num_permits: u64,
    ) -> Option<bool> {
        let table_entry = self.table_entries.get_mut(source_uid)?;
        let scaling_rate_limiter = match scaling_mode {
            ScalingMode::Up => &mut table_entry.scaling_up_rate_limiter,
            ScalingMode::Down => &mut table_entry.scaling_down_rate_limiter,
        };
        Some(scaling_rate_limiter.acquire(num_permits))
    }

    pub fn release_scaling_permits(
        &mut self,
        source_uid: &SourceUid,
        scaling_mode: ScalingMode,
        num_permits: u64,
    ) {
        if let Some(table_entry) = self.table_entries.get_mut(source_uid) {
            let scaling_rate_limiter = match scaling_mode {
                ScalingMode::Up => &mut table_entry.scaling_up_rate_limiter,
                ScalingMode::Down => &mut table_entry.scaling_down_rate_limiter,
            };
            scaling_rate_limiter.release(num_permits);
        }
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) struct ShardStats {
    pub num_open_shards: usize,
    pub avg_ingestion_rate: f32,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use itertools::Itertools;
    use quickwit_proto::ingest::Shard;

    use super::*;

    impl ShardTableEntry {
        pub fn shards(&self) -> Vec<Shard> {
            self.shard_entries
                .values()
                .map(|shard_entry| shard_entry.shard.clone())
                .sorted_unstable_by(|left, right| left.shard_id.cmp(&right.shard_id))
                .collect()
        }
    }

    impl ShardTable {
        pub fn find_open_shards_sorted(
            &self,
            index_uid: &IndexUid,
            source_id: &SourceId,
            unavailable_leaders: &FnvHashSet<NodeId>,
        ) -> Option<Vec<ShardEntry>> {
            self.find_open_shards(index_uid, source_id, unavailable_leaders)
                .map(|mut shards| {
                    shards.sort_unstable_by(|left, right| {
                        left.shard.shard_id.cmp(&right.shard.shard_id)
                    });
                    shards
                })
        }
    }

    #[test]
    fn test_shard_table_delete_index() {
        let mut shard_table = ShardTable::default();
        shard_table.delete_index("test-index");

        let index_uid_0: IndexUid = "test-index-foo:0".into();
        let source_id_0 = "test-source-0".to_string();
        shard_table.add_source(&index_uid_0, &source_id_0);

        let source_id_1 = "test-source-1".to_string();
        shard_table.add_source(&index_uid_0, &source_id_1);

        let index_uid_1: IndexUid = "test-index-bar:1".into();
        shard_table.add_source(&index_uid_1, &source_id_0);

        shard_table.delete_index("test-index-foo");
        assert_eq!(shard_table.table_entries.len(), 1);

        assert!(shard_table.table_entries.contains_key(&SourceUid {
            index_uid: index_uid_1,
            source_id: source_id_0
        }));
    }

    #[test]
    fn test_shard_table_add_source() {
        let index_uid: IndexUid = "test-index:0".into();
        let source_id = "test-source".to_string();

        let mut shard_table = ShardTable::default();
        shard_table.add_source(&index_uid, &source_id);
        assert_eq!(shard_table.table_entries.len(), 1);

        let source_uid = SourceUid {
            index_uid,
            source_id,
        };
        let table_entry = shard_table.table_entries.get(&source_uid).unwrap();
        assert!(table_entry.shard_entries.is_empty());
    }

    #[test]
    fn test_shard_table_list_shards() {
        let index_uid: IndexUid = "test-index:0".into();
        let source_id = "test-source".to_string();
        let source_uid = SourceUid {
            index_uid: index_uid.clone(),
            source_id: source_id.clone(),
        };
        let mut shard_table = ShardTable::default();

        assert!(shard_table.list_shards(&source_uid).is_none());

        shard_table.add_source(&index_uid, &source_id);
        let shards = shard_table.list_shards(&source_uid).unwrap();
        assert_eq!(shards.count(), 0);

        let shard_01 = Shard {
            index_uid: index_uid.clone().into(),
            source_id: source_id.clone(),
            shard_id: Some(ShardId::from(1)),
            leader_id: "test-leader-0".to_string(),
            shard_state: ShardState::Closed as i32,
            ..Default::default()
        };
        shard_table.insert_newly_opened_shards(&index_uid, &source_id, vec![shard_01]);

        let shards = shard_table.list_shards(&source_uid).unwrap();
        assert_eq!(shards.count(), 1);
    }

    #[test]
    fn test_shard_table_insert_newly_opened_shards() {
        let index_uid_0: IndexUid = "test-index:0".into();
        let source_id = "test-source".to_string();

        let mut shard_table = ShardTable::default();

        let shard_01 = Shard {
            index_uid: index_uid_0.clone().into(),
            source_id: source_id.clone(),
            shard_id: Some(ShardId::from(1)),
            leader_id: "test-leader-0".to_string(),
            shard_state: ShardState::Open as i32,
            ..Default::default()
        };
        shard_table.insert_newly_opened_shards(&index_uid_0, &source_id, vec![shard_01.clone()]);

        assert_eq!(shard_table.table_entries.len(), 1);

        let source_uid = SourceUid {
            index_uid: index_uid_0.clone(),
            source_id: source_id.clone(),
        };
        let table_entry = shard_table.table_entries.get(&source_uid).unwrap();
        let shards = table_entry.shards();
        assert_eq!(shards.len(), 1);
        assert_eq!(shards[0], shard_01);

        shard_table
            .table_entries
            .get_mut(&source_uid)
            .unwrap()
            .shard_entries
            .get_mut(&ShardId::from(1))
            .unwrap()
            .set_shard_state(ShardState::Unavailable);

        let shard_02 = Shard {
            index_uid: index_uid_0.clone().into(),
            source_id: source_id.clone(),
            shard_id: Some(ShardId::from(2)),
            leader_id: "test-leader-0".to_string(),
            shard_state: ShardState::Open as i32,
            ..Default::default()
        };

        shard_table.insert_newly_opened_shards(
            &index_uid_0,
            &source_id,
            vec![shard_01.clone(), shard_02.clone()],
        );

        assert_eq!(shard_table.table_entries.len(), 1);

        let source_uid = SourceUid {
            index_uid: index_uid_0.clone(),
            source_id: source_id.clone(),
        };
        let table_entry = shard_table.table_entries.get(&source_uid).unwrap();
        let shards = table_entry.shards();
        assert_eq!(shards.len(), 2);
        assert_eq!(shards[0].shard_state(), ShardState::Unavailable);
        assert_eq!(shards[1], shard_02);
    }

    #[test]
    fn test_shard_table_find_open_shards() {
        let index_uid: IndexUid = "test-index:0".into();
        let source_id = "test-source".to_string();

        let mut shard_table = ShardTable::default();
        shard_table.add_source(&index_uid, &source_id);

        let mut unavailable_ingesters = FnvHashSet::default();

        let open_shards = shard_table
            .find_open_shards_sorted(&index_uid, &source_id, &unavailable_ingesters)
            .unwrap();
        assert_eq!(open_shards.len(), 0);

        let shard_01 = Shard {
            index_uid: index_uid.clone().into(),
            source_id: source_id.clone(),
            shard_id: Some(ShardId::from(1)),
            leader_id: "test-leader-0".to_string(),
            shard_state: ShardState::Closed as i32,
            ..Default::default()
        };
        let shard_02 = Shard {
            index_uid: index_uid.clone().into(),
            source_id: source_id.clone(),
            shard_id: Some(ShardId::from(2)),
            leader_id: "test-leader-0".to_string(),
            shard_state: ShardState::Unavailable as i32,
            ..Default::default()
        };
        let shard_03 = Shard {
            index_uid: index_uid.clone().into(),
            source_id: source_id.clone(),
            shard_id: Some(ShardId::from(3)),
            leader_id: "test-leader-0".to_string(),
            shard_state: ShardState::Open as i32,
            ..Default::default()
        };
        let shard_04 = Shard {
            index_uid: index_uid.clone().into(),
            source_id: source_id.clone(),
            shard_id: Some(ShardId::from(4)),
            leader_id: "test-leader-1".to_string(),
            shard_state: ShardState::Open as i32,
            ..Default::default()
        };
        shard_table.insert_newly_opened_shards(
            &index_uid,
            &source_id,
            vec![shard_01, shard_02, shard_03.clone(), shard_04.clone()],
        );
        let open_shards = shard_table
            .find_open_shards_sorted(&index_uid, &source_id, &unavailable_ingesters)
            .unwrap();
        assert_eq!(open_shards.len(), 2);
        assert_eq!(open_shards[0].shard, shard_03);
        assert_eq!(open_shards[1].shard, shard_04);

        unavailable_ingesters.insert("test-leader-0".into());

        let open_shards = shard_table
            .find_open_shards_sorted(&index_uid, &source_id, &unavailable_ingesters)
            .unwrap();
        assert_eq!(open_shards.len(), 1);
        assert_eq!(open_shards[0].shard, shard_04);
    }

    #[test]
    fn test_shard_table_update_shards() {
        let index_uid: IndexUid = "test-index:0".into();
        let source_id = "test-source".to_string();

        let mut shard_table = ShardTable::default();

        let shard_01 = Shard {
            index_uid: index_uid.to_string(),
            source_id: source_id.clone(),
            shard_id: Some(ShardId::from(1)),
            shard_state: ShardState::Open as i32,
            ..Default::default()
        };
        let shard_02 = Shard {
            index_uid: index_uid.to_string(),
            source_id: source_id.clone(),
            shard_id: Some(ShardId::from(2)),
            shard_state: ShardState::Open as i32,
            ..Default::default()
        };
        let shard_03 = Shard {
            index_uid: index_uid.to_string(),
            source_id: source_id.clone(),
            shard_id: Some(ShardId::from(3)),
            shard_state: ShardState::Unavailable as i32,
            ..Default::default()
        };
        let shard_04 = Shard {
            index_uid: index_uid.to_string(),
            source_id: source_id.clone(),
            shard_id: Some(ShardId::from(4)),
            shard_state: ShardState::Open as i32,
            ..Default::default()
        };
        shard_table.insert_newly_opened_shards(
            &index_uid,
            &source_id,
            vec![shard_01, shard_02, shard_03, shard_04],
        );
        let source_uid = SourceUid {
            index_uid,
            source_id,
        };
        let shard_infos = BTreeSet::from_iter([
            ShardInfo {
                shard_id: ShardId::from(1),
                shard_state: ShardState::Open,
                ingestion_rate: RateMibPerSec(1),
            },
            ShardInfo {
                shard_id: ShardId::from(2),
                shard_state: ShardState::Open,
                ingestion_rate: RateMibPerSec(2),
            },
            ShardInfo {
                shard_id: ShardId::from(3),
                shard_state: ShardState::Open,
                ingestion_rate: RateMibPerSec(3),
            },
            ShardInfo {
                shard_id: ShardId::from(4),
                shard_state: ShardState::Closed,
                ingestion_rate: RateMibPerSec(4),
            },
            ShardInfo {
                shard_id: ShardId::from(5),
                shard_state: ShardState::Open,
                ingestion_rate: RateMibPerSec(5),
            },
        ]);
        let shard_stats = shard_table.update_shards(&source_uid, &shard_infos);
        assert_eq!(shard_stats.num_open_shards, 2);
        assert_eq!(shard_stats.avg_ingestion_rate, 1.5);

        let shard_entries: Vec<ShardEntry> = shard_table
            .list_shards(&source_uid)
            .unwrap()
            .cloned()
            .sorted_unstable_by(|left, right| left.shard.shard_id.cmp(&right.shard.shard_id))
            .collect();
        assert_eq!(shard_entries.len(), 4);

        assert_eq!(shard_entries[0].shard.shard_id(), ShardId::from(1));
        assert_eq!(shard_entries[0].shard.shard_state(), ShardState::Open);
        assert_eq!(shard_entries[0].ingestion_rate, RateMibPerSec(1));

        assert_eq!(shard_entries[1].shard.shard_id(), ShardId::from(2));
        assert_eq!(shard_entries[1].shard.shard_state(), ShardState::Open);
        assert_eq!(shard_entries[1].ingestion_rate, RateMibPerSec(2));

        assert_eq!(shard_entries[2].shard.shard_id(), ShardId::from(3));
        assert_eq!(
            shard_entries[2].shard.shard_state(),
            ShardState::Unavailable
        );
        assert_eq!(shard_entries[2].ingestion_rate, RateMibPerSec(3));

        assert_eq!(shard_entries[3].shard.shard_id(), ShardId::from(4));
        assert_eq!(shard_entries[3].shard.shard_state(), ShardState::Closed);
        assert_eq!(shard_entries[3].ingestion_rate, RateMibPerSec(4));
    }

    #[test]
    fn test_shard_table_close_shards() {
        let index_uid_0: IndexUid = "test-index:0".into();
        let index_uid_1: IndexUid = "test-index:1".into();
        let source_id = "test-source".to_string();

        let mut shard_table = ShardTable::default();

        let shard_01 = Shard {
            index_uid: index_uid_0.clone().into(),
            source_id: source_id.clone(),
            shard_id: Some(ShardId::from(1)),
            leader_id: "test-leader-0".to_string(),
            shard_state: ShardState::Open as i32,
            ..Default::default()
        };
        let shard_02 = Shard {
            index_uid: index_uid_0.clone().into(),
            source_id: source_id.clone(),
            shard_id: Some(ShardId::from(2)),
            leader_id: "test-leader-0".to_string(),
            shard_state: ShardState::Closed as i32,
            ..Default::default()
        };
        let shard_11 = Shard {
            index_uid: index_uid_1.clone().into(),
            source_id: source_id.clone(),
            shard_id: Some(ShardId::from(1)),
            leader_id: "test-leader-0".to_string(),
            shard_state: ShardState::Open as i32,
            ..Default::default()
        };
        shard_table.insert_newly_opened_shards(&index_uid_0, &source_id, vec![shard_01, shard_02]);
        shard_table.insert_newly_opened_shards(&index_uid_1, &source_id, vec![shard_11]);

        let source_uid_0 = SourceUid {
            index_uid: index_uid_0,
            source_id,
        };
        let closed_shard_ids = shard_table.close_shards(
            &source_uid_0,
            &[ShardId::from(1), ShardId::from(2), ShardId::from(3)],
        );
        assert_eq!(closed_shard_ids, &[ShardId::from(1)]);

        let table_entry = shard_table.table_entries.get(&source_uid_0).unwrap();
        let shards = table_entry.shards();
        assert_eq!(shards[0].shard_state(), ShardState::Closed);
    }

    #[test]
    fn test_shard_table_delete_shards() {
        let mut shard_table = ShardTable::default();

        let index_uid_0: IndexUid = "test-index:0".into();
        let index_uid_1: IndexUid = "test-index:1".into();
        let source_id = "test-source".to_string();

        let shard_01 = Shard {
            index_uid: index_uid_0.clone().into(),
            source_id: source_id.clone(),
            shard_id: Some(ShardId::from(1)),
            leader_id: "test-leader-0".to_string(),
            shard_state: ShardState::Open as i32,
            ..Default::default()
        };
        let shard_02 = Shard {
            index_uid: index_uid_0.clone().into(),
            source_id: source_id.clone(),
            shard_id: Some(ShardId::from(2)),
            leader_id: "test-leader-0".to_string(),
            shard_state: ShardState::Open as i32,
            ..Default::default()
        };
        let shard_11 = Shard {
            index_uid: index_uid_1.clone().into(),
            source_id: source_id.clone(),
            shard_id: Some(ShardId::from(1)),
            leader_id: "test-leader-0".to_string(),
            shard_state: ShardState::Open as i32,
            ..Default::default()
        };
        shard_table.insert_newly_opened_shards(
            &index_uid_0,
            &source_id,
            vec![shard_01.clone(), shard_02],
        );
        shard_table.insert_newly_opened_shards(&index_uid_1, &source_id, vec![shard_11]);

        let source_uid_0 = SourceUid {
            index_uid: index_uid_0.clone(),
            source_id: source_id.clone(),
        };
        shard_table.delete_shards(&source_uid_0, &[ShardId::from(2)]);

        let source_uid_1 = SourceUid {
            index_uid: index_uid_1.clone(),
            source_id: source_id.clone(),
        };
        shard_table.delete_shards(&source_uid_1, &[ShardId::from(1)]);

        assert_eq!(shard_table.table_entries.len(), 2);

        let table_entry = shard_table.table_entries.get(&source_uid_0).unwrap();
        let shards = table_entry.shards();
        assert_eq!(shards.len(), 1);
        assert_eq!(shards[0], shard_01);

        let table_entry = shard_table.table_entries.get(&source_uid_1).unwrap();
        assert!(table_entry.is_empty());
    }

    #[test]
    fn test_shard_table_acquire_scaling_up_permits() {
        let mut shard_table = ShardTable::default();

        let index_uid: IndexUid = "test-index:0".into();
        let source_id = "test-source".to_string();

        let source_uid = SourceUid {
            index_uid: index_uid.clone(),
            source_id: source_id.clone(),
        };
        assert!(shard_table
            .acquire_scaling_permits(&source_uid, ScalingMode::Up, 1)
            .is_none());

        shard_table.add_source(&index_uid, &source_id);

        let previous_available_permits = shard_table
            .table_entries
            .get(&source_uid)
            .unwrap()
            .scaling_up_rate_limiter
            .available_permits();

        assert!(shard_table
            .acquire_scaling_permits(&source_uid, ScalingMode::Up, 1)
            .unwrap());

        let new_available_permits = shard_table
            .table_entries
            .get(&source_uid)
            .unwrap()
            .scaling_up_rate_limiter
            .available_permits();

        assert_eq!(new_available_permits, previous_available_permits - 1);
    }

    #[test]
    fn test_shard_table_acquire_scaling_down_permits() {
        let index_uid: IndexUid = "test-index:0".into();
        let source_id = "test-source".to_string();

        let mut shard_table = ShardTable::default();

        let source_uid = SourceUid {
            index_uid: index_uid.clone(),
            source_id: source_id.clone(),
        };
        assert!(shard_table
            .acquire_scaling_permits(&source_uid, ScalingMode::Down, 1)
            .is_none());

        shard_table.add_source(&index_uid, &source_id);

        let previous_available_permits = shard_table
            .table_entries
            .get(&source_uid)
            .unwrap()
            .scaling_down_rate_limiter
            .available_permits();

        assert!(shard_table
            .acquire_scaling_permits(&source_uid, ScalingMode::Down, 1)
            .unwrap());

        let new_available_permits = shard_table
            .table_entries
            .get(&source_uid)
            .unwrap()
            .scaling_down_rate_limiter
            .available_permits();

        assert_eq!(new_available_permits, previous_available_permits - 1);
    }

    #[test]
    fn test_shard_table_release_scaling_up_permits() {
        let mut shard_table = ShardTable::default();

        let index_uid: IndexUid = "test-index:0".into();
        let source_id = "test-source".to_string();

        shard_table.add_source(&index_uid, &source_id);

        let source_uid = SourceUid {
            index_uid: index_uid.clone(),
            source_id: source_id.clone(),
        };
        let previous_available_permits = shard_table
            .table_entries
            .get(&source_uid)
            .unwrap()
            .scaling_up_rate_limiter
            .available_permits();

        assert!(shard_table
            .acquire_scaling_permits(&source_uid, ScalingMode::Up, 1)
            .unwrap());

        shard_table.release_scaling_permits(&source_uid, ScalingMode::Up, 1);

        let new_available_permits = shard_table
            .table_entries
            .get(&source_uid)
            .unwrap()
            .scaling_up_rate_limiter
            .available_permits();

        assert_eq!(new_available_permits, previous_available_permits);
    }

    #[test]
    fn test_shard_table_release_scaling_down_permits() {
        let mut shard_table = ShardTable::default();

        let index_uid: IndexUid = "test-index:0".into();
        let source_id = "test-source".to_string();

        shard_table.add_source(&index_uid, &source_id);

        let source_uid = SourceUid {
            index_uid: index_uid.clone(),
            source_id: source_id.clone(),
        };
        let previous_available_permits = shard_table
            .table_entries
            .get(&source_uid)
            .unwrap()
            .scaling_up_rate_limiter
            .available_permits();

        assert!(shard_table
            .acquire_scaling_permits(&source_uid, ScalingMode::Down, 1)
            .unwrap());

        shard_table.release_scaling_permits(&source_uid, ScalingMode::Down, 1);

        let new_available_permits = shard_table
            .table_entries
            .get(&source_uid)
            .unwrap()
            .scaling_up_rate_limiter
            .available_permits();

        assert_eq!(new_available_permits, previous_available_permits);
    }
}
