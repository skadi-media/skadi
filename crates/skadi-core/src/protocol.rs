//! Acquisition transport protocol.

use serde::{Deserialize, Serialize};

/// How a release is transported. Gates which indexers feed which downloaders
/// (a torrent indexer's releases go to a torrent downloader, etc.).
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub enum Protocol {
    Torrent,
    Usenet,
}
