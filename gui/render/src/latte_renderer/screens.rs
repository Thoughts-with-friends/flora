//! What the render thread shows the front end: screen images and memory shortages.
use super::*;

/// Tells other threads that video memory ran out while drawing large.
#[derive(Debug, Clone, Default)]
pub struct MemoryWatch(Arc<std::sync::atomic::AtomicU32>);

impl MemoryWatch {
    pub(super) fn report(&self, factor: u32) {
        self.0.store(factor, std::sync::atomic::Ordering::Relaxed);
    }

    /// The factor that did not fit, once for each time it happened.
    #[inline]
    pub fn take(&self) -> Option<u32> {
        Some(self.0.swap(0, std::sync::atomic::Ordering::Relaxed)).filter(|factor| *factor != 0)
    }
}

/// A screen image ready for presentation.
#[derive(Debug, Clone)]
pub struct ScreenImage {
    pub view: wgpu::TextureView,
    pub texture: wgpu::Texture,
    pub width: u32,
    pub height: u32,
    /// Changes whenever the texture object is replaced.
    pub generation: u64,
}

/// The latest image of each screen, readable from the UI thread.
#[derive(Debug, Default, Clone)]
pub struct ScreenImages(Arc<Mutex<ScreenSlots>>);

/// What each screen shows, and what it shows next from a given moment on.
#[derive(Debug, Default)]
struct ScreenSlots {
    shown: HashMap<Screen, ScreenImage>,
    next: HashMap<Screen, (ScreenImage, Instant)>,
}

impl ScreenImages {
    pub fn get(&self, screen: Screen) -> Option<ScreenImage> {
        let mut slots = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if slots.next.get(&screen).is_some_and(|(_, due)| *due <= Instant::now()) {
            if let Some((image, _)) = slots.next.remove(&screen) {
                slots.shown.insert(screen, image);
            }
        }
        slots.shown.get(&screen).cloned()
    }

    pub(crate) fn set(&self, screen: Screen, image: ScreenImage) {
        let mut slots = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        slots.next.remove(&screen);
        slots.shown.insert(screen, image);
    }

    /// `image` takes over at `due`.
    pub(crate) fn set_after(&self, screen: Screen, image: ScreenImage, due: Instant) {
        let mut slots = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        slots.next.insert(screen, (image, due));
    }
}
