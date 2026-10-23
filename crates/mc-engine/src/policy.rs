//! Which reconciler policy a session uses when the user chose none.

use mc_captions::ui::platform::Platform;
use mc_reconcile::Policy;
use mc_store::manifest::Manifest;

use crate::runs::parse_policy;

/// Browser caption UIs and desktop Teams show a speaker's sentence growing word by word:
/// V1 commits every growing prefix, V2 does not. Desktop Zoom keeps V1 until a real
/// meeting says otherwise.
pub fn default_policy(platform: &str) -> Policy {
    match Platform::parse(platform) {
        Some(Platform::Meet | Platform::TeamsWeb | Platform::Slack | Platform::Teams) => Policy::V2,
        _ => Policy::V1Legacy,
    }
}

/// The policy a live capture recorded in the manifest; sessions without one are V1.
pub fn recorded_policy(manifest: &Manifest) -> Policy {
    manifest
        .captions
        .extra
        .get("policy")
        .and_then(|v| v.as_str())
        .and_then(parse_policy)
        .unwrap_or(Policy::V1Legacy)
}
