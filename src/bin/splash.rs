//! Boot splash of the pedalboard OS: draws a PNG logo, centered on the
//! app background, straight on the Linux framebuffer (/dev/fb0). Started
//! early at boot (/etc/init.d/S01splash); the UI replaces it when it takes
//! the display over.
//!
//! Usage: pedalboard-splash <image.png> [rotation: 0 | 90 | 180 | 270]
//!
//! The rotation turns the picture clockwise, like SLINT_KMS_ROTATION.

/// Theme.background of the UI
#[cfg(target_os = "linux")]
const BACKGROUND: [u8; 3] = [0x05, 0x05, 0x06];

/// How long to watch the framebuffer: the firmware's one (simplefb, Pi) is
/// replaced by the display driver's a moment later, blank, so the picture
/// is drawn again on each new framebuffer
#[cfg(target_os = "linux")]
const WATCH: std::time::Duration = std::time::Duration::from_secs(10);

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(image) = args.next() else {
        eprintln!("usage: pedalboard-splash <image.png> [0|90|180|270]");
        std::process::exit(2);
    };
    let rotation = match args.next().as_deref() {
        Some("90") => 90,
        Some("180") => 180,
        Some("270") => 270,
        _ => 0,
    };
    if let Err(e) = run(&image, rotation) {
        eprintln!("pedalboard-splash: {e}");
        std::process::exit(1);
    }
}

#[cfg(not(target_os = "linux"))]
fn run(_image: &str, _rotation: u32) -> Result<(), String> {
    Err("needs the Linux framebuffer".into())
}

#[cfg(target_os = "linux")]
fn run(image: &str, rotation: u32) -> Result<(), String> {
    let logo = Logo::load(image).map_err(|e| format!("{image}: {e}"))?;
    let start = std::time::Instant::now();
    let mut shown = None;
    let mut error = None;
    while start.elapsed() < WATCH {
        if let Some(fb) = Framebuffer::read() {
            let id = (fb.name.clone(), fb.width, fb.height, fb.bpp);
            if shown.as_ref() != Some(&id) {
                match draw(&fb, &logo, rotation) {
                    Ok(()) => shown = Some(id),
                    Err(e) => error = Some(e),
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    match (shown, error) {
        (Some(_), _) => Ok(()),
        (None, Some(e)) => Err(e),
        (None, None) => Err("no framebuffer (/dev/fb0)".into()),
    }
}

#[cfg(target_os = "linux")]
fn draw(fb: &Framebuffer, logo: &Logo, rotation: u32) -> Result<(), String> {
    use std::io::Write;

    let bytes = fb.bytes_per_pixel();
    if !(2..=4).contains(&bytes) {
        return Err(format!("{} bits per pixel not supported", fb.bpp));
    }

    // Screen in the orientation of the picture
    let (width, height) = match rotation {
        90 | 270 => (fb.height, fb.width),
        _ => (fb.width, fb.height),
    };
    let scale = (width as f32 / logo.width as f32)
        .min(height as f32 / logo.height as f32)
        .min(1.0);
    let (logo_w, logo_h) = (
        (logo.width as f32 * scale) as usize,
        (logo.height as f32 * scale) as usize,
    );
    let (left, top) = ((width - logo_w) / 2, (height - logo_h) / 2);

    let mut device = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&fb.device)
        .map_err(|e| format!("{}: {e}", fb.device))?;
    let mut var = ScreenInfo::get(&device).map_err(|e| format!("{}: {e}", fb.device))?;
    let mut frame = vec![0u8; fb.stride * fb.height];
    for py in 0..fb.height {
        for px in 0..fb.width {
            let (x, y) = match rotation {
                90 => (py, fb.width - 1 - px),
                180 => (fb.width - 1 - px, fb.height - 1 - py),
                270 => (fb.height - 1 - py, px),
                _ => (px, py),
            };
            let mut rgb = BACKGROUND;
            if (left..left + logo_w).contains(&x) && (top..top + logo_h).contains(&y) {
                let sx = ((x - left) as f32 / scale) as usize;
                let sy = ((y - top) as f32 / scale) as usize;
                rgb = logo.over(sx.min(logo.width - 1), sy.min(logo.height - 1), rgb);
            }
            let at = py * fb.stride + px * bytes;
            frame[at..at + bytes].copy_from_slice(&var.pixel(rgb).to_le_bytes()[..bytes]);
        }
    }

    // The text console shares the framebuffer: keep it from drawing over
    // the picture (boot messages, cursor)
    console_graphics();

    device
        .write_all(&frame)
        .map_err(|e| format!("{}: {e}", fb.device))?;
    // Show it: without a text console, nothing has set the display mode
    var.activate();
    var.put(&device).map_err(|e| format!("{}: {e}", fb.device))
}

/// struct fb_var_screeninfo (linux/fb.h)
#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Default)]
struct ScreenInfo {
    geometry: [u32; 8],
    /// red, green, blue, transparency: offset, length, msb_right
    colors: [[u32; 3]; 4],
    nonstd: u32,
    activate: u32,
    timings: [u32; 14],
    reserved: [u32; 4],
}

#[cfg(target_os = "linux")]
impl ScreenInfo {
    const FBIOGET_VSCREENINFO: libc::c_ulong = 0x4600;
    const FBIOPUT_VSCREENINFO: libc::c_ulong = 0x4601;
    const FB_ACTIVATE_FORCE: u32 = 128;

    fn get(device: &std::fs::File) -> std::io::Result<Self> {
        let mut var = Self::default();
        Self::ioctl(device, Self::FBIOGET_VSCREENINFO, &mut var)?;
        Ok(var)
    }

    fn put(&mut self, device: &std::fs::File) -> Result<(), String> {
        Self::ioctl(device, Self::FBIOPUT_VSCREENINFO, self).map_err(|e| e.to_string())
    }

    /// Apply now, even unchanged (FB_ACTIVATE_NOW is 0)
    fn activate(&mut self) {
        self.activate = Self::FB_ACTIVATE_FORCE;
    }

    fn ioctl(
        device: &std::fs::File,
        request: libc::c_ulong,
        var: &mut Self,
    ) -> std::io::Result<()> {
        use std::os::fd::AsRawFd;
        // SAFETY: both requests take a struct fb_var_screeninfo, which Self
        // matches; the fd is open
        let r = unsafe { libc::ioctl(device.as_raw_fd(), request as _, var as *mut Self) };
        if r < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    /// Pixel value of a colour, opaque
    fn pixel(&self, rgb: [u8; 3]) -> u32 {
        let channel = |[offset, length, _]: [u32; 3], value: u8| {
            let length = length.min(8);
            (u32::from(value) >> (8 - length)) << offset
        };
        let [r, g, b, a] = self.colors;
        channel(r, rgb[0]) | channel(g, rgb[1]) | channel(b, rgb[2]) | channel(a, 0xff)
    }
}

/// RGBA picture
#[cfg(target_os = "linux")]
struct Logo {
    width: usize,
    height: usize,
    rgba: Vec<u8>,
}

#[cfg(target_os = "linux")]
impl Logo {
    fn load(path: &str) -> Result<Self, String> {
        let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
        decoder.set_transformations(png::Transformations::normalize_to_color8());
        let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
        let size = reader.output_buffer_size().ok_or("picture too large")?;
        let mut buf = vec![0; size];
        let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
        buf.truncate(info.buffer_size());
        let rgba = match info.color_type {
            png::ColorType::Rgba => buf,
            png::ColorType::Rgb => buf
                .chunks(3)
                .flat_map(|p| [p[0], p[1], p[2], 0xff])
                .collect(),
            png::ColorType::GrayscaleAlpha => buf
                .chunks(2)
                .flat_map(|p| [p[0], p[0], p[0], p[1]])
                .collect(),
            png::ColorType::Grayscale => buf.iter().flat_map(|&v| [v, v, v, 0xff]).collect(),
            png::ColorType::Indexed => return Err("unexpected indexed colors".into()),
        };
        if info.width == 0 || info.height == 0 {
            return Err("empty picture".into());
        }
        Ok(Self {
            width: info.width as usize,
            height: info.height as usize,
            rgba,
        })
    }

    /// Pixel (x, y) blended over `under`
    fn over(&self, x: usize, y: usize, under: [u8; 3]) -> [u8; 3] {
        let p = &self.rgba[(y * self.width + x) * 4..][..4];
        let a = u16::from(p[3]);
        std::array::from_fn(|i| {
            ((u16::from(p[i]) * a + u16::from(under[i]) * (255 - a)) / 255) as u8
        })
    }
}

#[cfg(target_os = "linux")]
struct Framebuffer {
    device: String,
    /// Driver: simple (firmware), vc4drmfb, virtio_gpudrmfb...
    name: String,
    width: usize,
    height: usize,
    bpp: usize,
    stride: usize,
}

#[cfg(target_os = "linux")]
impl Framebuffer {
    fn read() -> Option<Self> {
        let sys =
            |name: &str| std::fs::read_to_string(format!("/sys/class/graphics/fb0/{name}")).ok();
        let number = |s: &str| s.trim().parse::<usize>().ok();
        let size = sys("virtual_size")?;
        let (width, height) = size.trim().split_once(',')?;
        let fb = Self {
            device: "/dev/fb0".into(),
            name: sys("name")?.trim().to_string(),
            width: number(width)?,
            height: number(height)?,
            bpp: number(&sys("bits_per_pixel")?)?,
            stride: number(&sys("stride")?)?,
        };
        let usable = fb.width > 0
            && fb.height > 0
            && fb.stride >= fb.width * fb.bytes_per_pixel()
            && std::path::Path::new(&fb.device).exists();
        usable.then_some(fb)
    }

    fn bytes_per_pixel(&self) -> usize {
        self.bpp.div_ceil(8)
    }
}

/// Switch the text console to graphics mode (stops fbcon drawing on it)
#[cfg(target_os = "linux")]
fn console_graphics() {
    use std::os::fd::AsRawFd;
    const KDSETMODE: libc::c_ulong = 0x4B3A;
    const KD_GRAPHICS: libc::c_int = 1;
    if let Ok(tty) = std::fs::OpenOptions::new().write(true).open("/dev/tty0") {
        // SAFETY: KDSETMODE takes an int by value; the fd is open
        unsafe { libc::ioctl(tty.as_raw_fd(), KDSETMODE as _, KD_GRAPHICS) };
    }
}
