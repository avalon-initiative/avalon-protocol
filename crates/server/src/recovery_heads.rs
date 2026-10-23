//! Builds the cosigned-head source a node's indexer proves recovery delays with.

use std::sync::Arc;

use avalon_indexer::head_attestor::{HeadAttestor, SqlHeadAttestor};

use crate::known_list::KnownListHandle;

/// Heads count once enough of the known list's witnesses cosigned them: a majority, or none at all
/// when the list has at most one member (the author's signature alone, as for any head).
pub fn head_attestor(
    network_id: &str,
    own_shard_id: &str,
    known_list: KnownListHandle,
) -> Arc<dyn HeadAttestor> {
    let members = known_list.clone();
    Arc::new(SqlHeadAttestor::new(
        network_id,
        own_shard_id,
        Arc::new(move || {
            let size = known_list.len();
            if size <= 1 {
                0
            } else {
                avalon_protocol::witness::majority_threshold(size)
            }
        }),
        Arc::new(move || members.witness_key_ids()),
    ))
}
