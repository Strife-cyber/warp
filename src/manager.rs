use std::sync::Arc;
use crate::segment::Chunk;

pub struct Metadata {
    pub url: String,
    pub size: u64,
    pub chunks: Vec<Arc<Chunk>>
}

