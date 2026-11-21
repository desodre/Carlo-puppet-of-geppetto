//! The seam between `serve` and a live video source.
//!
//! `serve` only knows [`Source`]: something that, on the thread that calls it,
//! produces encoder output into a [`Consumer`] until the consumer stops wanting
//! frames. The one real implementation is [`Mirror`], the profile-gated display
//! mirror session; host tests inject a synthetic one. Nothing here touches a
//! socket or a platform API.

use super::mirror::{self, Consumer, Profile};
use crate::{diagnostics::Diagnostics, error::CarloError};

/// A validated `video.start` configuration: what the encoder is configured with
/// and what the response reports as accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Params {
    /// Encoder width in pixels: even, 16..=1920.
    pub(crate) width: u16,
    /// Encoder height in pixels: even, 16..=1920.
    pub(crate) height: u16,
    /// Frames per second, 1..=60.
    pub(crate) fps: u32,
    /// Bits per second, 100_000..=20_000_000.
    pub(crate) bitrate: u32,
}

/// Key frame interval of every stream, in seconds.
pub(crate) const KEY_FRAME_INTERVAL_SECONDS: i32 = mirror::STREAM_KEY_FRAME_INTERVAL_SECONDS;

/// A producer of one H.264 stream of the main display at a time.
///
/// `run` is called on a dedicated thread and must create, use and drop every
/// platform object on that thread. `Send + Sync` covers only the immutable
/// description of the source, never those objects.
pub(crate) trait Source: Send + Sync {
    /// Whether this target can stream at all (the profile gate).
    fn available(&self) -> bool;

    /// Produce one stream into `consumer`: call [`Consumer::started`] once the
    /// display and encoder exist, push output while [`Consumer::wanted`], then
    /// release everything and push the end of stream.
    ///
    /// # Errors
    /// A stable protocol code (`permission_denied`, `display_absent`,
    /// `unsupported_format`, `display_refused`, `timeout`, `internal`); never a
    /// platform message.
    fn run(&self, params: &Params, consumer: &mut dyn Consumer) -> Result<(), &'static str>;
}

/// The display mirror of the pinned build. Unavailable everywhere else.
pub(crate) struct Mirror(Option<Profile>);

impl Mirror {
    /// Apply the mirror profile gate to this process; makes no Binder call.
    pub(crate) fn detect(environment: &Diagnostics) -> Result<Self, CarloError> {
        let fingerprint = crate::accessibility::fingerprint()?;
        Ok(Self(Profile::detect(environment, fingerprint.as_deref())))
    }
}

impl Source for Mirror {
    fn available(&self) -> bool {
        self.0.is_some()
    }

    fn run(&self, params: &Params, consumer: &mut dyn Consumer) -> Result<(), &'static str> {
        let profile = self.0.as_ref().ok_or("internal")?;
        let bitrate = i32::try_from(params.bitrate).map_err(|_| "internal")?;
        let fps = i32::try_from(params.fps).map_err(|_| "internal")?;
        mirror::stream(
            profile,
            i32::from(params.width),
            i32::from(params.height),
            fps,
            bitrate,
            consumer,
        )
        .map_err(|error| mirror::failure_code(&error))
    }
}

#[cfg(all(test, not(target_os = "android")))]
mod tests {
    use super::*;

    #[test]
    fn mirror_source_is_unavailable_off_the_pinned_build() {
        let host = Diagnostics::collect().unwrap();
        assert!(!Mirror::detect(&host).unwrap().available());
        // The exact shell environment without the pinned fingerprint is refused too.
        let shell = Diagnostics::from_environment(
            "android",
            "aarch64",
            Some(36),
            Some("arm64-v8a".into()),
            Some(2000),
            None,
        );
        assert!(Profile::detect(&shell, None).is_none());
        assert!(Profile::detect(&shell, Some("another/build:16/X/1:user/release-keys")).is_none());
        assert!(Profile::detect(&host, None).is_none());
    }
}
