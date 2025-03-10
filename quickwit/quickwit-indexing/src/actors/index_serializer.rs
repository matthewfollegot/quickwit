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

use async_trait::async_trait;
use quickwit_actors::{Actor, ActorContext, ActorExitStatus, Handler, Mailbox, QueueCapacity};
use quickwit_common::io::IoControls;
use quickwit_common::runtimes::RuntimeType;
use tokio::runtime::Handle;
use tracing::instrument;

use crate::actors::Packager;
use crate::models::{EmptySplit, IndexedSplit, IndexedSplitBatch, IndexedSplitBatchBuilder};

/// The index serializer takes a non-serialized split,
/// and serializes it before passing it to the packager.
///
/// This is usually a CPU heavy operation.
///
/// Depending on the data
/// (terms cardinality) and the index settings (sorted or not)
/// it can range from medium IO to IO heavy.
pub struct IndexSerializer {
    packager_mailbox: Mailbox<Packager>,
}

impl IndexSerializer {
    pub fn new(packager_mailbox: Mailbox<Packager>) -> Self {
        Self { packager_mailbox }
    }
}

#[async_trait]
impl Actor for IndexSerializer {
    type ObservableState = ();

    fn observable_state(&self) -> Self::ObservableState {}

    fn queue_capacity(&self) -> QueueCapacity {
        QueueCapacity::Bounded(0)
    }

    fn runtime_handle(&self) -> Handle {
        RuntimeType::Blocking.get_runtime_handle()
    }
}

#[async_trait]
impl Handler<IndexedSplitBatchBuilder> for IndexSerializer {
    type Reply = ();

    #[instrument(
        name="serialize_split_batch"
        parent=batch_builder.batch_parent_span.id(),
        skip_all,
    )]
    async fn handle(
        &mut self,
        batch_builder: IndexedSplitBatchBuilder,
        ctx: &ActorContext<Self>,
    ) -> Result<(), ActorExitStatus> {
        let mut splits: Vec<IndexedSplit> = Vec::with_capacity(batch_builder.splits.len());
        for split_builder in batch_builder.splits {
            // TODO Consider & test removing this protect guard.
            //
            // In theory the controlled directory should be sufficient.
            let _protect_guard = ctx.protect_zone();
            if let Some(controlled_directory) = &split_builder.controlled_directory_opt {
                let io_controls = IoControls::default()
                    .set_progress(ctx.progress().clone())
                    .set_kill_switch(ctx.kill_switch().clone())
                    .set_component("index_serializer");
                controlled_directory.set_io_controls(io_controls);
            }
            let split = split_builder.finalize()?;
            splits.push(split);
        }
        let indexed_split_batch = IndexedSplitBatch {
            splits,
            checkpoint_delta_opt: batch_builder.checkpoint_delta_opt,
            publish_lock: batch_builder.publish_lock,
            publish_token_opt: batch_builder.publish_token_opt,
            merge_operation_opt: None,
            batch_parent_span: batch_builder.batch_parent_span,
        };
        ctx.send_message(&self.packager_mailbox, indexed_split_batch)
            .await?;
        Ok(())
    }
}

#[async_trait]
impl Handler<EmptySplit> for IndexSerializer {
    type Reply = ();

    #[instrument(
        name="serialize_empty_split"
        parent=empty_split.batch_parent_span.id(),
        skip_all,
    )]
    async fn handle(
        &mut self,
        empty_split: EmptySplit,
        ctx: &ActorContext<Self>,
    ) -> Result<(), ActorExitStatus> {
        ctx.send_message(&self.packager_mailbox, empty_split)
            .await?;
        Ok(())
    }
}
