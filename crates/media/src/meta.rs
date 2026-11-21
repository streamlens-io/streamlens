//! `AnalysisMeta`: attaches an `Arc<FrameAnalysis>` to decoded buffers.
//!
//! In-process only — the payload is a Rust struct behind an `Arc`, so the
//! meta is cheap to transform (buffer copies share the analysis).

use std::sync::Arc;
use std::{fmt, mem};

use gstreamer as gst;
use gstreamer::prelude::*;
use streamlens_types::FrameAnalysis;

#[repr(transparent)]
pub struct AnalysisMeta(imp::AnalysisMeta);

unsafe impl Send for AnalysisMeta {}
unsafe impl Sync for AnalysisMeta {}

impl AnalysisMeta {
    pub fn add(
        buffer: &mut gst::BufferRef,
        analysis: Arc<FrameAnalysis>,
    ) -> gst::MetaRefMut<'_, Self, gst::meta::Standalone> {
        unsafe {
            // gst_buffer_add_meta takes ownership of the params content.
            let mut params = mem::ManuallyDrop::new(imp::AnalysisMetaParams { analysis });
            let meta = gst::ffi::gst_buffer_add_meta(
                buffer.as_mut_ptr(),
                imp::analysis_meta_get_info(),
                &mut *params as *mut imp::AnalysisMetaParams as gst::glib::ffi::gpointer,
            ) as *mut imp::AnalysisMeta;
            Self::from_mut_ptr(buffer, meta)
        }
    }

    pub fn analysis(&self) -> &Arc<FrameAnalysis> {
        &self.0.analysis
    }
}

unsafe impl MetaAPI for AnalysisMeta {
    type GstType = imp::AnalysisMeta;

    fn meta_api() -> gst::glib::Type {
        imp::analysis_meta_api_get_type()
    }
}

impl fmt::Debug for AnalysisMeta {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("AnalysisMeta")
            .field("poc", &self.0.analysis.poc)
            .field("superblocks", &self.0.analysis.superblocks.len())
            .finish()
    }
}

mod imp {
    use super::*;
    use gst::glib::translate::*;
    use std::ptr;
    use std::sync::LazyLock;

    pub(super) struct AnalysisMetaParams {
        pub analysis: Arc<FrameAnalysis>,
    }

    #[repr(C)]
    pub struct AnalysisMeta {
        parent: gst::ffi::GstMeta,
        pub(super) analysis: Arc<FrameAnalysis>,
    }

    pub(super) fn analysis_meta_api_get_type() -> gst::glib::Type {
        static TYPE: LazyLock<gst::glib::Type> = LazyLock::new(|| unsafe {
            let t = from_glib(gst::ffi::gst_meta_api_type_register(
                c"StreamlensAnalysisMetaAPI".as_ptr() as *const _,
                [ptr::null::<std::ffi::c_char>()].as_ptr() as *mut *const _,
            ));
            assert_ne!(t, gst::glib::Type::INVALID);
            t
        });
        *TYPE
    }

    unsafe extern "C" fn analysis_meta_init(
        meta: *mut gst::ffi::GstMeta,
        params: gst::glib::ffi::gpointer,
        _buffer: *mut gst::ffi::GstBuffer,
    ) -> gst::glib::ffi::gboolean {
        unsafe {
            assert!(!params.is_null());
            let meta = &mut *(meta as *mut AnalysisMeta);
            let params = ptr::read(params as *const AnalysisMetaParams);
            ptr::write(&mut meta.analysis, params.analysis);
            true.into_glib()
        }
    }

    unsafe extern "C" fn analysis_meta_free(
        meta: *mut gst::ffi::GstMeta,
        _buffer: *mut gst::ffi::GstBuffer,
    ) {
        unsafe {
            let meta = &mut *(meta as *mut AnalysisMeta);
            ptr::drop_in_place(&mut meta.analysis);
        }
    }

    unsafe extern "C" fn analysis_meta_transform(
        dest: *mut gst::ffi::GstBuffer,
        meta: *mut gst::ffi::GstMeta,
        _buffer: *mut gst::ffi::GstBuffer,
        _type_: gst::glib::ffi::GQuark,
        _data: gst::glib::ffi::gpointer,
    ) -> gst::glib::ffi::gboolean {
        unsafe {
            let meta = &*(meta as *mut AnalysisMeta);
            super::AnalysisMeta::add(gst::BufferRef::from_mut_ptr(dest), meta.analysis.clone());
            true.into_glib()
        }
    }

    pub(super) fn analysis_meta_get_info() -> *const gst::ffi::GstMetaInfo {
        struct MetaInfo(ptr::NonNull<gst::ffi::GstMetaInfo>);
        unsafe impl Send for MetaInfo {}
        unsafe impl Sync for MetaInfo {}

        static META_INFO: LazyLock<MetaInfo> = LazyLock::new(|| unsafe {
            MetaInfo(
                ptr::NonNull::new(gst::ffi::gst_meta_register(
                    analysis_meta_api_get_type().into_glib(),
                    c"StreamlensAnalysisMeta".as_ptr() as *const _,
                    mem::size_of::<AnalysisMeta>(),
                    Some(analysis_meta_init),
                    Some(analysis_meta_free),
                    Some(analysis_meta_transform),
                ) as *mut gst::ffi::GstMetaInfo)
                .expect("register AnalysisMeta"),
            )
        });
        META_INFO.0.as_ptr()
    }
}
