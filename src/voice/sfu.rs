use crate::state::AppState;

/// Allocate the best SFU node for a given region.
pub fn allocate_node(
    state: &AppState,
    preferred_region: Option<&str>,
) -> Option<crate::state::SfuNode> {
    let mut best: Option<crate::state::SfuNode> = None;
    let mut best_load = i64::MAX;

    for entry in state.sfu_nodes.iter() {
        let node = entry.value();
        if node.status != "online" {
            continue;
        }
        if let Some(region) = preferred_region {
            if node.region == region && node.current_load < best_load {
                best = Some(node.clone());
                best_load = node.current_load;
            }
        } else if node.current_load < best_load {
            best = Some(node.clone());
            best_load = node.current_load;
        }
    }

    best
}
