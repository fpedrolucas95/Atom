//! Image cache for Atom OS.
//!
//! Stores decoded images (DecodedImage) to avoid re-decoding from disk.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use crate::image::DecodedImage;

pub struct ImageCache {
    images: Vec<(String, Arc<DecodedImage>)>,
    max_entries: usize,
}

impl ImageCache {
    pub const DEFAULT_MAX_ENTRIES: usize = 8;

    pub fn new() -> Self {
        Self::with_capacity(Self::DEFAULT_MAX_ENTRIES)
    }

    pub fn with_capacity(max_entries: usize) -> Self {
        Self {
            images: Vec::new(),
            max_entries: max_entries.max(1),
        }
    }

    pub fn get(&self, path: &str) -> Option<Arc<DecodedImage>> {
        self.images
            .iter()
            .find(|(cached_path, _)| cached_path == path)
            .map(|(_, image)| image.clone())
    }

    pub fn insert(&mut self, path: String, image: DecodedImage) {
        if let Some(pos) = self.images.iter().position(|(cached_path, _)| *cached_path == path) {
            self.images.remove(pos);
        } else if self.images.len() >= self.max_entries {
            self.images.remove(0);
        }

        self.images.push((path, Arc::new(image)));
    }

    pub fn len(&self) -> usize {
        self.images.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn mk_img(seed: u8) -> DecodedImage {
        DecodedImage::new(1, 1, vec![seed, seed, seed, 255])
    }

    #[test]
    fn cache_returns_inserted_image() {
        let mut cache = ImageCache::with_capacity(2);
        cache.insert(String::from("/system/wallpapers/a.jpg"), mk_img(1));
        assert!(cache.get("/system/wallpapers/a.jpg").is_some());
    }

    #[test]
    fn cache_evicts_oldest_entry() {
        let mut cache = ImageCache::with_capacity(2);
        cache.insert(String::from("/system/wallpapers/a.jpg"), mk_img(1));
        cache.insert(String::from("/system/wallpapers/b.jpg"), mk_img(2));
        cache.insert(String::from("/system/wallpapers/c.jpg"), mk_img(3));

        assert!(cache.get("/system/wallpapers/a.jpg").is_none());
        assert!(cache.get("/system/wallpapers/b.jpg").is_some());
        assert!(cache.get("/system/wallpapers/c.jpg").is_some());
    }

    #[test]
    fn cache_reinsertion_refreshes_entry() {
        let mut cache = ImageCache::with_capacity(2);
        cache.insert(String::from("/system/wallpapers/a.jpg"), mk_img(1));
        cache.insert(String::from("/system/wallpapers/b.jpg"), mk_img(2));
        cache.insert(String::from("/system/wallpapers/a.jpg"), mk_img(3));
        cache.insert(String::from("/system/wallpapers/c.jpg"), mk_img(4));

        assert!(cache.get("/system/wallpapers/a.jpg").is_some());
        assert!(cache.get("/system/wallpapers/b.jpg").is_none());
        assert_eq!(cache.len(), 2);
    }
}
