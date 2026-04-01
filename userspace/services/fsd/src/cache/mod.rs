// cache/mod.rs
pub mod lru;
pub mod fat_cache;
pub mod cluster_cache;

pub use fat_cache::FatCache;
pub use cluster_cache::ClusterCache;
