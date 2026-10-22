//! Host-side LV2 features handed to plugins at instantiation:
//! urid:map/unmap, options (block length, sample rate), buf-size and the
//! worker (`work:schedule`, see `lv2_worker`).

use lv2_raw::core::LV2Feature;

use super::atom::{ATOM_FLOAT, ATOM_INT};
use super::lv2_worker;
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};
use std::sync::{Mutex, OnceLock};

pub const URID_MAP: &str = "http://lv2plug.in/ns/ext/urid#map";
pub const URID_UNMAP: &str = "http://lv2plug.in/ns/ext/urid#unmap";
pub const OPTIONS: &str = "http://lv2plug.in/ns/ext/options#options";
pub const BOUNDED_BLOCK_LENGTH: &str = "http://lv2plug.in/ns/ext/buf-size#boundedBlockLength";
pub const IS_LIVE: &str = "http://lv2plug.in/ns/lv2core#isLive";

pub const ATOM_SEQUENCE: &str = "http://lv2plug.in/ns/ext/atom#Sequence";
pub const ATOM_CHUNK: &str = "http://lv2plug.in/ns/ext/atom#Chunk";
const MIN_BLOCK_LENGTH: &str = "http://lv2plug.in/ns/ext/buf-size#minBlockLength";
const MAX_BLOCK_LENGTH: &str = "http://lv2plug.in/ns/ext/buf-size#maxBlockLength";
const NOMINAL_BLOCK_LENGTH: &str = "http://lv2plug.in/ns/ext/buf-size#nominalBlockLength";
const SAMPLE_RATE: &str = "http://lv2plug.in/ns/ext/parameters#sampleRate";

/// Features this host provides; plugins requiring anything else are skipped.
pub const SUPPORTED_FEATURES: &[&str] = &[
    URID_MAP,
    URID_UNMAP,
    OPTIONS,
    BOUNDED_BLOCK_LENGTH,
    IS_LIVE,
    lv2_worker::SCHEDULE,
    "http://lv2plug.in/ns/lv2core#hardRTCapable",
    "http://lv2plug.in/ns/lv2core#inPlaceBroken",
];

/// Process-wide URI ↔ URID table. IDs must be stable for the whole process
/// because plugins cache them.
struct UridTable {
    ids: HashMap<CString, u32>,
    uris: Vec<CString>,
}

fn table() -> &'static Mutex<UridTable> {
    static TABLE: OnceLock<Mutex<UridTable>> = OnceLock::new();
    TABLE.get_or_init(|| {
        Mutex::new(UridTable {
            ids: HashMap::new(),
            uris: Vec::new(),
        })
    })
}

/// Map a URI to its URID (0 is reserved for "no URID")
pub fn map_uri(uri: &str) -> u32 {
    let uri = CString::new(uri).unwrap_or_default();
    map_cstr(&uri)
}

fn map_cstr(uri: &CStr) -> u32 {
    let mut table = table().lock().unwrap();
    if let Some(&id) = table.ids.get(uri) {
        return id;
    }
    table.uris.push(uri.to_owned());
    let id = table.uris.len() as u32;
    table.ids.insert(uri.to_owned(), id);
    id
}

extern "C" fn urid_map(_handle: *mut c_void, uri: *const c_char) -> u32 {
    if uri.is_null() {
        return 0;
    }
    map_cstr(unsafe { CStr::from_ptr(uri) })
}

extern "C" fn urid_unmap(_handle: *mut c_void, urid: u32) -> *const c_char {
    let table = table().lock().unwrap();
    match urid.checked_sub(1).and_then(|i| table.uris.get(i as usize)) {
        // The CString heap buffer never moves or gets freed, so the pointer
        // stays valid after the lock is released.
        Some(uri) => uri.as_ptr(),
        None => std::ptr::null(),
    }
}

#[repr(C)]
struct UridMap {
    handle: *mut c_void,
    map: extern "C" fn(*mut c_void, *const c_char) -> u32,
}

#[repr(C)]
struct UridUnmap {
    handle: *mut c_void,
    unmap: extern "C" fn(*mut c_void, u32) -> *const c_char,
}

/// LV2_Options_Option
#[repr(C)]
struct OptionsOption {
    context: u32, // LV2_OPTIONS_INSTANCE = 0
    subject: u32,
    key: u32,
    size: u32,
    value_type: u32,
    value: *const c_void,
}

/// Owns every feature struct and the null-terminated pointer list passed to
/// `instantiate`. Must outlive the plugin instance: plugins may keep pointers.
pub struct FeatureSet {
    _uris: Vec<CString>,
    _map: Box<UridMap>,
    _unmap: Box<UridUnmap>,
    _options: Box<[OptionsOption]>,
    _option_values: Box<[i32; 3]>,
    _sample_rate: Box<f32>,
    worker: lv2_worker::Feature,
    features: Vec<LV2Feature>,
}

// Raw pointers here only point into data owned by the FeatureSet itself.
unsafe impl Send for FeatureSet {}

impl FeatureSet {
    pub fn new(sample_rate: f32, max_block: usize) -> Self {
        let map = Box::new(UridMap {
            handle: std::ptr::null_mut(),
            map: urid_map,
        });
        let unmap = Box::new(UridUnmap {
            handle: std::ptr::null_mut(),
            unmap: urid_unmap,
        });

        let max_block = max_block.min(i32::MAX as usize) as i32;
        let option_values = Box::new([1i32, max_block, max_block]);
        let sample_rate = Box::new(sample_rate);
        let int_type = map_uri(ATOM_INT);
        let int_option = |key: &str, value: &i32| OptionsOption {
            context: 0,
            subject: 0,
            key: map_uri(key),
            size: std::mem::size_of::<i32>() as u32,
            value_type: int_type,
            value: value as *const i32 as *const c_void,
        };
        let options: Box<[OptionsOption]> = Box::new([
            int_option(MIN_BLOCK_LENGTH, &option_values[0]),
            int_option(MAX_BLOCK_LENGTH, &option_values[1]),
            int_option(NOMINAL_BLOCK_LENGTH, &option_values[2]),
            OptionsOption {
                context: 0,
                subject: 0,
                key: map_uri(SAMPLE_RATE),
                size: std::mem::size_of::<f32>() as u32,
                value_type: map_uri(ATOM_FLOAT),
                value: &*sample_rate as *const f32 as *const c_void,
            },
            // Zeroed terminator
            OptionsOption {
                context: 0,
                subject: 0,
                key: 0,
                size: 0,
                value_type: 0,
                value: std::ptr::null(),
            },
        ]);

        let worker = lv2_worker::Feature::new();
        let uris: Vec<CString> = [
            URID_MAP,
            URID_UNMAP,
            OPTIONS,
            BOUNDED_BLOCK_LENGTH,
            IS_LIVE,
            lv2_worker::SCHEDULE,
        ]
        .iter()
        .map(|u| CString::new(*u).unwrap())
        .collect();
        let data: [*mut c_void; 6] = [
            &*map as *const UridMap as *mut c_void,
            &*unmap as *const UridUnmap as *mut c_void,
            options.as_ptr() as *mut c_void,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            worker.data(),
        ];
        let features = uris
            .iter()
            .zip(data)
            .map(|(uri, data)| LV2Feature {
                uri: uri.as_ptr(),
                data,
            })
            .collect();

        Self {
            _uris: uris,
            _map: map,
            _unmap: unmap,
            _options: options,
            _option_values: option_values,
            _sample_rate: sample_rate,
            worker,
            features,
        }
    }

    pub fn features(&self) -> &[LV2Feature] {
        &self.features
    }

    /// The `work:schedule` feature, to start the worker of the instance
    pub fn worker(&self) -> &lv2_worker::Feature {
        &self.worker
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urid_map_roundtrip() {
        let a = map_uri("urn:test:a");
        let b = map_uri("urn:test:b");
        assert_ne!(a, 0);
        assert_ne!(a, b);
        assert_eq!(a, map_uri("urn:test:a"));
        let back = unsafe { CStr::from_ptr(urid_unmap(std::ptr::null_mut(), b)) };
        assert_eq!(back.to_str().unwrap(), "urn:test:b");
        assert!(urid_unmap(std::ptr::null_mut(), 0).is_null());
    }
}
