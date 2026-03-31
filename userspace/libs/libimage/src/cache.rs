//! Image cache for Atom OS.
//!
//! Stores decoded images (DecodedImage) to avoid re-decoding from disk.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use crate::image::DecodedImage;

pub struct ImageCache {
    images: BTreeMap<String, Arc<DecodedImage>>,
}

impl ImageCache {
    pub fn new() -> Self {
        Self {
            images: BTreeMap::new(),
        }
    }

    pub fn get(&self, path: &str) -> Option<Arc<DecodedImage>> {
        self.images.get(path).cloned()
    }

    pub fn insert(&mut self, path: String, image: DecodedImage) {
        self.images.insert(path, Arc::new(image));
    }
}
