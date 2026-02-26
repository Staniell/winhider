/*
 * =============================================================================
 * WinHider Application - Core Application
 * =============================================================================
 *
 * Filename: main.rs
 * Author: bigwiz
 * Description: Main application entry point for WinHider, a professional Windows
 *              window visibility controller. Provides GUI interface for hiding
 *              windows from screen capture and taskbar while maintaining normal
 *              usability.
 *
 * Features:
 * - Window enumeration and management
 * - Screen capture hiding via DLL injection
 * - Taskbar visibility control
 * - System tray integration
 * - Auto-update functionality
 * - Configurable preview quality
 * - Settings persistence
 *
 * Designed At - Bitmutex Technologies
 * =============================================================================
 */

#![windows_subsystem = "windows"]

use eframe::egui;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::ffi::c_void;
use std::time::{Duration, SystemTime};
use std::path::PathBuf;
use std::env;
use chrono::Datelike;

use windows::Win32::Foundation::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::Win32::Graphics::Gdi::*;

use winhider_core::{
    clean_temp_files, inject_payload, set_always_on_top, set_taskbar_visibility_external,
    InjectionAction,
};

use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS};
use windows::Win32::System::Com::{CoInitializeEx, CoCreateInstance, COINIT_APARTMENTTHREADED, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Shell::{ITaskbarList, TaskbarList};

// WGC Imports
use windows_capture::{
    capture::{CaptureControl, Context, GraphicsCaptureApiHandler},
    frame::Frame,
    monitor::Monitor,
    settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
        MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
    },
};

// ===============================
// CONSTANTS & CONFIG
// ===============================

const REPO_OWNER: &str = "aamitn";
const REPO_NAME: &str = "winhider";
const APP_NAME: &str = "WinHider";
const APP_VERSION_DEFAULT: &str = "v1.0.0";
const VERSION_FILE: &str = "appver.txt";
const USER_AGENT: &str = "WinHider-App";




// ===============================
// Data Models
// ===============================

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct AppSettings {
    enable_auto_update: bool,
    #[serde(default = "default_preview_quality")]
    preview_quality: u32,
    #[serde(default = "default_hotkey_enabled")]
    global_hotkey_enabled: bool,
    #[serde(default = "default_hotkey_vk")]
    global_hotkey_vk: u32,
}

fn default_preview_quality() -> u32 {
    2  // Default: Medium quality (scale factor 2)
}

fn default_hotkey_enabled() -> bool {
    true
}

fn default_hotkey_vk() -> u32 {
    0xC0 // VK_OEM_3 = backtick key
}

#[derive(Clone, PartialEq)]
enum UpdateStatus {
    Idle,
    Checking,
    UpToDate,
    UpdateAvailable(String), 
    Error(String),
}

#[derive(Clone)]
pub struct AppWindow {
    pub hwnd: HWND,
    pub pid: u32,
    pub title: String,
    pub is_taskbar_hidden: bool,
    pub is_capture_hidden: bool,
    pub is_always_on_top: bool,
    pub icon_texture: Option<egui::TextureHandle>,
}

// ===============================
// WGC HANDLER
// ===============================

struct WgcHandler {
    sender: crossbeam_channel::Sender<egui::ColorImage>,
    preview_quality: u32,
}

impl GraphicsCaptureApiHandler for WgcHandler {
    type Flags = crossbeam_channel::Sender<egui::ColorImage>;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self {
            sender: ctx.flags,
            preview_quality: 2,  // Default quality
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        _capture_control: windows_capture::graphics_capture_api::InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        if self.sender.is_full() {
            return Ok(());
        }

        let width = frame.width();
        let height = frame.height();
        
        if let Ok(mut buffer) = frame.buffer() {
            if let Ok(raw_slice) = buffer.as_nopadding_buffer() {
                // Nearest Neighbor Downscale (Fast)
                let scale_factor = 3; 
                let new_width = width / scale_factor;
                let new_height = height / scale_factor;
                
                let mut fast_buffer = Vec::with_capacity((new_width * new_height * 4) as usize);
                let stride = (width * 4) as usize; 
                
                for y in 0..new_height {
                    let src_y_offset = (y * scale_factor) as usize * stride;
                    for x in 0..new_width {
                        let src_x_offset = (x * scale_factor) as usize * 4;
                        let i = src_y_offset + src_x_offset;
                        
                        if i + 4 <= raw_slice.len() {
                            fast_buffer.extend_from_slice(&raw_slice[i..i+4]);
                        }
                    }
                }

                let egui_img = egui::ColorImage::from_rgba_unmultiplied(
                    [new_width as usize, new_height as usize],
                    &fast_buffer,
                );

                let _ = self.sender.try_send(egui_img);
            }
        }

        Ok(())
    }
}

// ===============================
// STEALTH FOCUS STATE
// ===============================

struct StealthFocusState {
    /// Last foreground window that was NOT capture-hidden
    cover_hwnd: HWND,
    /// Whether a capture-hidden window is currently foreground
    active: bool,
    /// Frame counter for heartbeat (re-send every 30 frames)
    heartbeat_counter: u32,
    /// COM ITaskbarList, initialized lazily
    taskbar_list: Option<ITaskbarList>,
}

impl StealthFocusState {
    fn get_taskbar_list(&mut self) -> Option<&ITaskbarList> {
        if self.taskbar_list.is_none() {
            unsafe {
                if let Ok(tbl) = CoCreateInstance::<_, ITaskbarList>(&TaskbarList, None, CLSCTX_INPROC_SERVER) {
                    let _ = tbl.HrInit();
                    self.taskbar_list = Some(tbl);
                }
            }
        }
        self.taskbar_list.as_ref()
    }
}

// ===============================
// MAIN APP STATE
// ===============================

struct WinHiderApp {
    // App Config
    app_version: String, 
    app_icon_texture: Option<egui::TextureHandle>, // NEW: Stores the loaded whicon.ico

    self_hide_capture: bool,
    self_hide_taskbar: bool,
    windows: Vec<AppWindow>,
    status_msg: String,
    last_refresh: SystemTime,
    
    // Preview Fields
    monitors: Vec<Monitor>,
    selected_monitor_idx: usize,
    show_preview: bool,
    preview_texture: Option<egui::TextureHandle>,
    preview_quality: u32,
    
    // UI State
    show_about_dialog: bool,
    show_update_dialog: bool,
    update_status: UpdateStatus,
    latest_version: Option<String>,
    auto_hide_list: Vec<String>,
    show_auto_hide_editor: bool,
    new_app_input: String,
    selected_window_idx: Vec<HWND>,

    // Settings
    enable_auto_update: bool,
    global_hotkey_enabled: bool,
    global_hotkey_vk: u32,

    // Self always-on-top (one-shot)
    self_topmost_applied: bool,

    // Stealth focus
    stealth_focus: StealthFocusState,

    // Global Hotkey
    hotkey_receiver: crossbeam_channel::Receiver<()>,
    hotkey_shutdown_sender: Option<crossbeam_channel::Sender<()>>,

    // Communication Channels
    capture_control: Option<CaptureControl<WgcHandler, Box<dyn std::error::Error + Send + Sync>>>,
    frame_receiver: crossbeam_channel::Receiver<egui::ColorImage>,
    frame_sender: crossbeam_channel::Sender<egui::ColorImage>, 
    
    update_sender: crossbeam_channel::Sender<UpdateStatus>,
    update_receiver: crossbeam_channel::Receiver<UpdateStatus>,
}

impl WinHiderApp {
    // REPLACED default() with new(cc) to handle texture loading
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        clean_temp_files();
        
        let app_version = std::fs::read_to_string(VERSION_FILE)
            .unwrap_or_else(|_| APP_VERSION_DEFAULT.to_string())
            .trim()
            .to_string();

        let settings = load_settings();

        // --- LOAD APP ICON FOR UI ---
        // This embeds the icon into the binary so it works even if the .ico file is deleted
        let icon_bytes = include_bytes!("../../Misc/whicon-small.ico"); 
        let image = image::load_from_memory(icon_bytes).expect("Failed to load whicon.ico");
        let size = [image.width() as _, image.height() as _];
        let image_buffer = image.to_rgba8();
        let pixels = image_buffer.as_flat_samples();
        let color_image = egui::ColorImage::from_rgba_unmultiplied(
            size,
            pixels.as_slice(),
        );
        let app_icon_texture = Some(cc.egui_ctx.load_texture("app_icon", color_image, egui::TextureOptions::LINEAR));

        let (tx, rx) = crossbeam_channel::bounded(1);
        let (up_tx, up_rx) = crossbeam_channel::unbounded();
        let monitors = Monitor::enumerate().unwrap_or_default();

        // Set up global hotkey channel
        let (hk_tx, hk_rx) = crossbeam_channel::bounded(1);
        let hotkey_shutdown = if settings.global_hotkey_enabled {
            let (shutdown_tx, shutdown_rx) = crossbeam_channel::bounded(1);
            start_hotkey_thread(settings.global_hotkey_vk, hk_tx.clone(), shutdown_rx);
            Some(shutdown_tx)
        } else {
            None
        };

        let mut app = Self {
            app_version,
            app_icon_texture,
            self_hide_capture: false,
            self_hide_taskbar: false,
            windows: Vec::new(),
            status_msg: "Ready.".to_string(),
            last_refresh: SystemTime::UNIX_EPOCH,

            monitors,
            selected_monitor_idx: 0,
            show_preview: true,
            preview_texture: None,
            preview_quality: settings.preview_quality,
            show_about_dialog: false,
            show_update_dialog: false,
            update_status: UpdateStatus::Idle,
            latest_version: None,
            auto_hide_list: load_auto_hide_list(),
            show_auto_hide_editor: false,
            new_app_input: String::new(),
            selected_window_idx: Vec::new(),
            enable_auto_update: settings.enable_auto_update,
            global_hotkey_enabled: settings.global_hotkey_enabled,
            global_hotkey_vk: settings.global_hotkey_vk,

            self_topmost_applied: false,
            stealth_focus: StealthFocusState {
                cover_hwnd: HWND(0),
                active: false,
                heartbeat_counter: 0,
                taskbar_list: None,
            },

            hotkey_receiver: hk_rx,
            hotkey_shutdown_sender: hotkey_shutdown,

            capture_control: None,
            frame_receiver: rx,
            frame_sender: tx,

            update_sender: up_tx,
            update_receiver: up_rx,
        };

        app.start_capture_session();
        
        if app.enable_auto_update {
            app.check_for_updates();
        }
        
        app
    }

    fn start_capture_session(&mut self) {
        if let Some(ctrl) = self.capture_control.take() {
            let _ = ctrl.stop();
        }

        if !self.show_preview || self.monitors.is_empty() {
            return;
        }

        if self.selected_monitor_idx >= self.monitors.len() {
            self.selected_monitor_idx = 0;
        }

        let monitor = self.monitors[self.selected_monitor_idx];
        let sender = self.frame_sender.clone();

        let settings = Settings::new(
            monitor,
            CursorCaptureSettings::Default,
            DrawBorderSettings::Default,
            SecondaryWindowSettings::Default,
            MinimumUpdateIntervalSettings::Default,
            DirtyRegionSettings::Default,
            ColorFormat::Rgba8,
            sender
        );

        if let Ok(ctrl) = WgcHandler::start_free_threaded(settings) {
            self.capture_control = Some(ctrl);
        } else {
            self.status_msg = "Failed to start Graphics Capture".to_string();
        }
    }

    fn check_for_updates(&mut self) {
        self.update_status = UpdateStatus::Checking;
        self.show_update_dialog = true;
        let sender = self.update_sender.clone();
        let current_version = self.app_version.clone();

        std::thread::spawn(move || {
            let url = format!("https://api.github.com/repos/{}/{}/releases/latest", REPO_OWNER, REPO_NAME);
            
            let resp = ureq::get(&url)
                .set("User-Agent", USER_AGENT)
                .call();

            match resp {
                Ok(response) => {
                    if let Ok(json) = response.into_json::<serde_json::Value>() {
                        if let Some(tag_name) = json["tag_name"].as_str() {
                            if is_version_newer(&current_version, tag_name) {
                                let _ = sender.send(UpdateStatus::UpdateAvailable(tag_name.to_string()));
                            } else {
                                let _ = sender.send(UpdateStatus::UpToDate);
                            }
                            return;
                        }
                    }
                    let _ = sender.send(UpdateStatus::Error("Invalid response format".to_string()));
                },
                Err(e) => {
                    let _ = sender.send(UpdateStatus::Error(e.to_string()));
                }
            }
        });
    }
}

fn start_hotkey_thread(
    vk: u32,
    hotkey_sender: crossbeam_channel::Sender<()>,
    shutdown_receiver: crossbeam_channel::Receiver<()>,
) {
    std::thread::spawn(move || unsafe {
        const HOTKEY_ID: i32 = 1;
        let _ = RegisterHotKey(HWND(0), HOTKEY_ID, HOT_KEY_MODIFIERS(0), vk);

        loop {
            // Check for shutdown signal
            if shutdown_receiver.try_recv().is_ok() {
                break;
            }

            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, HWND(0), 0, 0, PM_REMOVE).into() {
                if msg.message == WM_HOTKEY {
                    let _ = hotkey_sender.try_send(());
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        let _ = UnregisterHotKey(HWND(0), HOTKEY_ID);
    });
}

impl Drop for WinHiderApp {
    fn drop(&mut self) {
        // Signal hotkey thread to shut down
        if let Some(sender) = self.hotkey_shutdown_sender.take() {
            let _ = sender.send(());
        }
    }
}

// ===============================
// egui App
// ===============================

impl eframe::App for WinHiderApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let self_hwnd = get_eframe_hwnd(frame);

        // --- One-shot: make WinHider always-on-top ---
        if !self.self_topmost_applied {
            let _ = set_always_on_top(self_hwnd, true);
            // Initialize cover_hwnd to the current foreground if it's not ourselves
            let fg = unsafe { GetForegroundWindow() };
            if fg != self_hwnd && fg.0 != 0 {
                self.stealth_focus.cover_hwnd = fg;
            }
            self.self_topmost_applied = true;
        }

        // --- Global Hotkey: toggle WinHider visibility ---
        if self.hotkey_receiver.try_recv().is_ok() {
            unsafe {
                let fg = GetForegroundWindow();
                if fg == self_hwnd {
                    // Already focused -> minimize/hide
                    let _ = ShowWindow(self_hwnd, SW_MINIMIZE);
                } else {
                    // Not focused -> show and bring to front
                    let _ = ShowWindow(self_hwnd, SW_RESTORE);
                    let _ = SetForegroundWindow(self_hwnd);
                    let _ = set_always_on_top(self_hwnd, true); // re-apply after restore
                }
            }
        }

        // --- Stealth Focus: keep cover window looking active ---
        self.update_stealth_focus(self_hwnd);

        // --- 1. Background Logic ---
        if let Ok(elapsed) = SystemTime::now().duration_since(self.last_refresh) {
            if elapsed > Duration::from_secs(2) {
                let new_windows = enumerate_windows(ctx);
                let mut merged = Vec::new();
                for mut w in new_windows {
                    if let Some(old) = self.windows.iter().find(|o| o.hwnd == w.hwnd) {
                        w.is_taskbar_hidden = old.is_taskbar_hidden;
                        w.is_capture_hidden = old.is_capture_hidden;
                        w.is_always_on_top = old.is_always_on_top;
                    } else {
                        // Auto-hide new windows if they match the list
                        if self.should_auto_hide(&w.title) {
                            w.is_taskbar_hidden = true;
                            w.is_capture_hidden = true;
                            let _ = set_taskbar_visibility_external(w.hwnd, true);
                            let _ = inject_payload(w.hwnd, InjectionAction::HideCapture);
                        }
                    }
                    merged.push(w);
                }
                self.windows = merged;
                self.last_refresh = SystemTime::now();
            }
        }

        // --- 2. WGC Frame Receiver ---
        if self.show_preview {
            if let Ok(img) = self.frame_receiver.try_recv() {
                self.preview_texture = Some(ctx.load_texture(
                    "screen_preview",
                    img,
                    egui::TextureOptions::LINEAR
                ));
            }
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(Duration::from_secs(1));
        }

        // Check update results
        if let Ok(status) = self.update_receiver.try_recv() {
            self.update_status = status.clone();
            // Extract latest version if available
            match &self.update_status {
                UpdateStatus::UpdateAvailable(version) => {
                    self.latest_version = Some(version.clone());
                }
                UpdateStatus::UpToDate => {
                    self.latest_version = Some(self.app_version.clone());
                }
                _ => {}
            }
            // Auto-close if no update
            if matches!(self.update_status, UpdateStatus::UpToDate | UpdateStatus::Error(_)) {
                self.show_update_dialog = false;
            }
        }

        // --- Hotkey Handling ---
        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::S)) {
            if !self.selected_window_idx.is_empty() {
                let mut success_count = 0;
                for &selected_hwnd in &self.selected_window_idx {
                    if let Some(window) = self.windows.iter_mut().find(|w| w.hwnd == selected_hwnd) {
                        let action = if window.is_capture_hidden {
                            InjectionAction::ShowCapture
                        } else {
                            InjectionAction::HideCapture
                        };
                        if inject_payload(window.hwnd, action).is_ok() {
                            window.is_capture_hidden = !window.is_capture_hidden;
                            success_count += 1;
                        }
                    }
                }
                if success_count > 0 {
                    self.status_msg = format!("Ctrl+S: Toggled capture for {} windows", success_count);
                } else {
                    self.status_msg = "Ctrl+S: Failed to toggle capture".to_string();
                }
            } else {
                self.status_msg = "Ctrl+S: No windows selected".to_string();
            }
        }

        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::T)) {
            if !self.selected_window_idx.is_empty() {
                let mut success_count = 0;
                for &selected_hwnd in &self.selected_window_idx {
                    if let Some(window) = self.windows.iter_mut().find(|w| w.hwnd == selected_hwnd) {
                        let hide = !window.is_taskbar_hidden;
                        if set_taskbar_visibility_external(window.hwnd, hide).is_ok() {
                            window.is_taskbar_hidden = hide;
                            success_count += 1;
                        }
                    }
                }
                if success_count > 0 {
                    self.status_msg = format!("Ctrl+T: Toggled taskbar for {} windows", success_count);
                } else {
                    self.status_msg = "Ctrl+T: Failed to toggle taskbar".to_string();
                }
            } else {
                self.status_msg = "Ctrl+T: No windows selected".to_string();
            }
        }

        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::P)) {
            if !self.selected_window_idx.is_empty() {
                let mut success_count = 0;
                for &selected_hwnd in &self.selected_window_idx {
                    if let Some(window) = self.windows.iter_mut().find(|w| w.hwnd == selected_hwnd) {
                        let pin = !window.is_always_on_top;
                        if set_always_on_top(window.hwnd, pin).is_ok() {
                            window.is_always_on_top = pin;
                            success_count += 1;
                        }
                    }
                }
                if success_count > 0 {
                    self.status_msg = format!("Ctrl+P: Toggled pin for {} windows", success_count);
                } else {
                    self.status_msg = "Ctrl+P: Failed to toggle pin".to_string();
                }
            } else {
                self.status_msg = "Ctrl+P: No windows selected".to_string();
            }
        }

        // --- 3. MENU BAR ---
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Launch CLI").clicked() {
                        if let Err(e) = launch_cli() {
                            self.status_msg = format!("Failed to launch CLI: {}", e);
                        } else {
                            self.status_msg = "CLI launched successfully.".to_string();
                        }
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Clear Temp Files").clicked() {
                        clean_temp_files();
                        self.status_msg = "Temporary injection files cleaned.".to_string();
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Exit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });

                ui.menu_button("Settings", |ui| {
                    if ui.checkbox(&mut self.enable_auto_update, "Enable Auto-Updates").changed() {
                        let _ = save_settings(&self.current_settings());
                    }

                    ui.separator();

                    if ui.checkbox(&mut self.global_hotkey_enabled, "Enable Focus Hotkey (`)").changed() {
                        if self.global_hotkey_enabled {
                            // Start hotkey thread
                            let (hk_tx, hk_rx) = crossbeam_channel::bounded(1);
                            let (shutdown_tx, shutdown_rx) = crossbeam_channel::bounded(1);
                            start_hotkey_thread(self.global_hotkey_vk, hk_tx, shutdown_rx);
                            self.hotkey_receiver = hk_rx;
                            self.hotkey_shutdown_sender = Some(shutdown_tx);
                        } else {
                            // Stop hotkey thread
                            if let Some(sender) = self.hotkey_shutdown_sender.take() {
                                let _ = sender.send(());
                            }
                        }
                        let _ = save_settings(&self.current_settings());
                    }

                    ui.separator();
                    ui.label(egui::RichText::new("Preview Quality").strong());

                    let quality_options = [(1, "Low (Fastest)"), (2, "Medium (Balanced)"), (3, "High (Best)")];
                    for (value, label) in quality_options.iter() {
                        if ui.selectable_value(&mut self.preview_quality, *value, *label).changed() {
                            let _ = save_settings(&self.current_settings());
                            self.start_capture_session();
                        }
                    }
                });

                ui.menu_button("Help", |ui| {
                    if ui.button("Check for Updates").clicked() {
                        self.check_for_updates();
                        ui.close_menu();
                    }
                    if ui.button("About").clicked() {
                        self.show_about_dialog = true;
                        ui.close_menu();
                    }
                });
            });
        });

        // --- 4. CENTRAL PANEL ---
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.heading(egui::RichText::new(APP_NAME).size(24.0).strong().color(egui::Color32::from_rgb(120, 200, 255)));
                        ui.label(egui::RichText::new(&self.app_version).size(11.0).color(egui::Color32::from_rgb(120, 200, 255)).weak());
                    });
                    ui.label(egui::RichText::new("Bitmutex Technologies").small().color(egui::Color32::YELLOW));
                });
                
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.checkbox(&mut self.show_preview, "Show Preview").changed() {
                        self.start_capture_session(); 
                    }
                    
                    if !self.monitors.is_empty() {
                         let combo = egui::ComboBox::from_id_source("monitor_select")
                            .selected_text(format!("Monitor {}", self.selected_monitor_idx + 1))
                            .show_ui(ui, |ui| {
                                for (i, _m) in self.monitors.iter().enumerate() {
                                    if ui.selectable_value(&mut self.selected_monitor_idx, i, format!("Monitor {}", i + 1)).clicked() {
                                        return true; 
                                    }
                                }
                                false
                            });
                        
                        if let Some(true) = combo.inner {
                            self.start_capture_session();
                        }
                    }
                });
            });
            
            ui.separator();

            if self.show_preview {
                let default_width = ui.available_width().min(500.0);
                // Set default height to minimum (150px) instead of full size
                let default_height = 150.0;
                egui::Resize::default()
                    .default_size([default_width, default_height])
                    .min_size([default_width, 100.0])
                    .max_size([default_width, 600.0])
                    .show(ui, |ui| {
                        ui.centered_and_justified(|ui| {
                            if let Some(texture) = &self.preview_texture {
                                let pane_size = ui.available_size();
                                let aspect = 16.0 / 9.0;
                                let image_size = if pane_size.x / pane_size.y > aspect {
                                    // Pane is wider than 16:9, fit to height
                                    egui::vec2(pane_size.y * aspect, pane_size.y)
                                } else {
                                    // Pane is taller than 16:9, fit to width
                                    egui::vec2(pane_size.x, pane_size.x / aspect)
                                };
                                ui.image((texture.id(), image_size));
                            } else {
                                ui.label("Waiting for WGC Stream...");
                            }
                        });
                    });
                ui.add_space(10.0);
            }

            ui.add_space(10.0);

            ui.horizontal(|ui| {
                ui.group(|ui| {
                    ui.label(egui::RichText::new("Auto-Hide on Start").strong());
                    ui.vertical(|ui| {
                        ui.label(format!("{} apps configured", self.auto_hide_list.len()));
                        if ui.button("Edit List").clicked() {
                            self.show_auto_hide_editor = true;
                        }
                    });
                });

                ui.group(|ui| {
                    ui.label(egui::RichText::new("Local Stealth").strong());
                    ui.vertical(|ui| {
                        if ui.checkbox(&mut self.self_hide_capture, "Hide Self from Capture").changed() {
                            let _ = set_capture_self(self_hwnd, self.self_hide_capture);
                        }
                        if ui.checkbox(&mut self.self_hide_taskbar, "Hide Self from Taskbar").changed() {
                            let _ = set_taskbar_visibility_external(self_hwnd, self.self_hide_taskbar);
                        }
                    });
                });
            });

            ui.add_space(10.0);

            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Target Applications").strong());
                if ui.button("🔄 Force Refresh").clicked() {
                    self.windows = enumerate_windows(ctx);
                    self.monitors = Monitor::enumerate().unwrap_or_default();
                    self.last_refresh = SystemTime::now();
                    self.selected_window_idx.clear(); // Reset selection on refresh
                    self.status_msg = "List refreshed.".to_string();
                }
            });

            ui.add_space(5.0);
            ui.label(egui::RichText::new(&self.status_msg).color(egui::Color32::LIGHT_BLUE));
            ui.label(egui::RichText::new("Hotkeys: Ctrl+S=Toggle Capture, Ctrl+T=Toggle Taskbar, Ctrl+P=Toggle Pin (select windows first, Ctrl+click for multi-select)").small().color(egui::Color32::GRAY));
            ui.separator();

            egui::ScrollArea::vertical()
                .auto_shrink([false, false]) 
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());

                    for window in &mut self.windows {
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                // 1. Icon
                                if let Some(texture) = &window.icon_texture {
                                    ui.image((texture.id(), egui::vec2(16.0, 16.0)));
                                } else {
                                    ui.label("⬜");
                                }

                                // 2. Prepare Mixed-Style Text (Title + PID)
                                let display_title = truncate_middle(&window.title, 45); // Adjust length as needed
                                let is_selected = self.selected_window_idx.contains(&window.hwnd);

                                // Create a LayoutJob to mix styles in one clickable label
                                let mut job = egui::text::LayoutJob::default();
                                
                                // Part A: Title (White, Proportional)
                                job.append(
                                    &display_title,
                                    0.0,
                                    egui::TextFormat {
                                        font_id: egui::FontId::proportional(14.0), // Normal UI font
                                        color: egui::Color32::WHITE,
                                        ..Default::default()
                                    },
                                );

                                // Part B: PID (Gray, Monospace, Smaller)
                                job.append(
                                    &format!(" ({})", window.pid),
                                    0.0,
                                    egui::TextFormat {
                                        font_id: egui::FontId::monospace(12.0), // Code font
                                        color: egui::Color32::GRAY,
                                        ..Default::default()
                                    },
                                );

                                // 3. Render Selectable Label with Mixed Text
                                let sel_resp = ui.selectable_label(is_selected, job);
                                if sel_resp.hovered() {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::Default);
                                }
                                if sel_resp.clicked() {
                                    // Handle Multi-selection (Ctrl+Click) vs Single Selection
                                    if ui.input(|i| i.modifiers.ctrl) {
                                        if is_selected {
                                            self.selected_window_idx.retain(|&h| h != window.hwnd);
                                        } else {
                                            self.selected_window_idx.push(window.hwnd);
                                        }
                                    } else {
                                        if is_selected && self.selected_window_idx.len() == 1 {
                                            self.selected_window_idx.clear();
                                        } else {
                                            self.selected_window_idx.clear();
                                            self.selected_window_idx.push(window.hwnd);
                                        }
                                    }
                                    self.status_msg = format!("Selected: {} windows", self.selected_window_idx.len());
                                }

                                // 4. Selection Indicator
                                if is_selected {
                                    ui.label(egui::RichText::new("← Selected").small().color(egui::Color32::YELLOW));
                                }
                            });

                            ui.horizontal(|ui| {
                                let tb_resp = ui.checkbox(&mut window.is_taskbar_hidden, "Hide Taskbar");
                                if tb_resp.hovered() {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::Default);
                                }
                                if tb_resp.changed() {
                                    if let Err(e) = set_taskbar_visibility_external(window.hwnd, window.is_taskbar_hidden) {
                                        self.status_msg = format!("Error: {}", e);
                                        window.is_taskbar_hidden = !window.is_taskbar_hidden;
                                    }
                                }

                                ui.separator();

                                let cap_resp = ui.checkbox(&mut window.is_capture_hidden, "Hide Capture");
                                if cap_resp.hovered() {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::Default);
                                }
                                if cap_resp.changed() {
                                    let action = if window.is_capture_hidden {
                                        InjectionAction::HideCapture
                                    } else {
                                        InjectionAction::ShowCapture
                                    };
                                    match inject_payload(window.hwnd, action) {
                                        Ok(_) => self.status_msg = format!("Capture state updated: {}", window.title),
                                        Err(e) => {
                                            self.status_msg = format!("Error: {}", e);
                                            window.is_capture_hidden = !window.is_capture_hidden;
                                        }
                                    }
                                }

                                ui.separator();

                                let top_resp = ui.checkbox(&mut window.is_always_on_top, "Pin on Top");
                                if top_resp.hovered() {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::Default);
                                }
                                if top_resp.changed() {
                                    match set_always_on_top(window.hwnd, window.is_always_on_top) {
                                        Ok(_) => self.status_msg = format!("Pin state updated: {}", window.title),
                                        Err(e) => {
                                            self.status_msg = format!("Error: {}", e);
                                            window.is_always_on_top = !window.is_always_on_top;
                                        }
                                    }
                                }
                            });
                        });
                    }
                });
        });

        // --- UPDATE DIALOG ---
        if self.show_update_dialog {
            let mut is_open = true;
            let mut should_close = false;

            egui::Window::new("Check for Updates")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .open(&mut is_open)
                .show(ctx, |ui| {
                    ui.vertical_centered(|ui| {
                        match &self.update_status {
                            UpdateStatus::Checking => {
                                ui.spinner();
                                ui.label("Checking GitHub for updates...");
                            },
                            UpdateStatus::UpToDate => {
                                ui.label(egui::RichText::new("✓ You are up to date!").color(egui::Color32::GREEN));
                                ui.label(format!("Current version: {}", self.app_version));
                                if ui.button("Close").clicked() {
                                    should_close = true;
                                }
                            },
                            UpdateStatus::UpdateAvailable(new_tag) => {
                                ui.heading(egui::RichText::new("New Version Available!").color(egui::Color32::YELLOW));
                                ui.label(format!("Current: {}", self.app_version));
                                ui.label(format!("Latest:  {}", new_tag));
                                ui.add_space(10.0);
                                
                                if ui.button(egui::RichText::new("⬇ Download Installer").size(16.0)).clicked() {
                                    let download_url = format!(
                                        "https://github.com/{}/{}/releases/download/{}/WinhiderInstaller.exe",
                                        REPO_OWNER, REPO_NAME, new_tag
                                    );
                                    let _ = std::process::Command::new("cmd").args(["/C", "start", &download_url]).spawn();
                                }
                            },
                            UpdateStatus::Error(err) => {
                                ui.label(egui::RichText::new("⚠ Check Failed").color(egui::Color32::RED));
                                ui.label(err);
                                if ui.button("Close").clicked() {
                                    should_close = true;
                                }
                            },
                            _ => {}
                        }
                    });
                });
            
            if !is_open || should_close {
                self.show_update_dialog = false;
            }
        }

        // --- ABOUT DIALOG ---
        if self.show_about_dialog {
            let mut is_open = true;

            egui::Window::new(format!("About {}", APP_NAME))
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .default_width(340.0)
                .open(&mut is_open)
                .show(ctx, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(6.0);

                        // App Icon
                        if let Some(texture) = &self.app_icon_texture {
                            ui.image((texture.id(), egui::vec2(72.0, 72.0)));
                            ui.add_space(8.0);
                        }

                        // App Name
                        ui.label(
                            egui::RichText::new(APP_NAME)
                                .size(22.0)
                                .strong(),
                        );

                        // Version
                        ui.label(
                            egui::RichText::new(format!("Version {}", self.app_version))
                                .size(13.0)
                                .color(egui::Color32::GRAY),
                        );

                        // Latest Available Version
                        if let Some(latest) = &self.latest_version {
                            let color = if is_version_newer(&self.app_version, latest) {
                                egui::Color32::from_rgb(255, 165, 0) // Orange for update available
                            } else {
                                egui::Color32::GREEN // Green for up to date
                            };
                            ui.label(
                                egui::RichText::new(format!("Latest: {}", latest))
                                    .size(12.0)
                                    .color(color),
                            );
                        } else {
                            ui.label(
                                egui::RichText::new("Latest: Checking...")
                                    .size(12.0)
                                    .color(egui::Color32::GRAY),
                            );
                        }

                        ui.add_space(10.0);
                        ui.separator();
                        ui.add_space(10.0);

                        // Description block
                        ui.label(
                            egui::RichText::new("Professional Window Visibility Controller")
                                .strong(),
                        );

                        ui.add_space(6.0);

                        ui.label(
                            egui::RichText::new(
                                "WinHider allows advanced control over how application windows \
                                appear to the system and screen capture software."
                            )
                            .color(egui::Color32::from_gray(200)),
                        );

                        ui.add_space(6.0);

                        ui.label("• Hide windows from screen capture (OBS, Teams, Zoom)");
                        ui.label("• Remove windows from Taskbar and Alt-Tab");
                        ui.label("• Maintain normal usability while hidden");
                        ui.label("• Auto-Hide on startup based on custom list");

                        ui.add_space(14.0);
                        ui.separator();
                        ui.add_space(6.0);

                        let current_year = chrono::Local::now().year();

                        ui.label(
                            egui::RichText::new(format!("© {} Bitmutex Technologies", current_year))
                                .size(12.0)
                                .color(egui::Color32::GRAY),
                        );

                        ui.add_space(4.0);
                    });
                });

            if !is_open {
                self.show_about_dialog = false;
            }
        }

        // --- AUTO-HIDE EDITOR DIALOG ---
        if self.show_auto_hide_editor {
            let mut is_open = true;
            let mut should_save = false;

            egui::Window::new("Auto-Hide Applications")
                .collapsible(false)
                .resizable(true)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .default_width(500.0)
                .default_height(400.0)
                .open(&mut is_open)
                .show(ctx, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.label("Applications to automatically hide on startup:");
                        ui.add_space(10.0);
                    });

                    ui.separator();

                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .max_height(200.0)
                        .show(ui, |ui| {
                            let mut to_remove = None;
                            for (i, app) in self.auto_hide_list.iter().enumerate() {
                                ui.horizontal(|ui| {
                                    ui.label(app);
                                    if ui.button("❌").on_hover_text("Remove").clicked() {
                                        to_remove = Some(i);
                                    }
                                });
                            }
                            if let Some(i) = to_remove {
                                self.auto_hide_list.remove(i);
                            }
                        });

                    ui.separator();
                    ui.add_space(10.0);

                    ui.horizontal(|ui| {
                        ui.label("Add application:");
                        let response = ui.text_edit_singleline(&mut self.new_app_input);
                        if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            if !self.new_app_input.trim().is_empty() && !self.auto_hide_list.contains(&self.new_app_input) {
                                self.auto_hide_list.push(self.new_app_input.trim().to_string());
                                self.new_app_input.clear();
                            }
                        }
                        if ui.button("Add").clicked() {
                            if !self.new_app_input.trim().is_empty() && !self.auto_hide_list.contains(&self.new_app_input) {
                                self.auto_hide_list.push(self.new_app_input.trim().to_string());
                                self.new_app_input.clear();
                            }
                        }
                    });

                    ui.add_space(20.0);
                    ui.separator();

                    ui.horizontal(|ui| {
                        if ui.button("Save & Close").clicked() {
                            should_save = true;
                        }
                        if ui.button("Cancel").clicked() {
                            // Reload the list to discard changes
                            self.auto_hide_list = load_auto_hide_list();
                        }
                    });
                });

            if !is_open || should_save {
                if should_save {
                    if let Err(e) = save_auto_hide_list(&self.auto_hide_list) {
                        self.status_msg = format!("Failed to save auto-hide list: {}", e);
                    } else {
                        self.status_msg = "Auto-hide list saved.".to_string();
                    }
                }
                self.show_auto_hide_editor = false;
            }
        }

    }
}

// ===============================
// UTILITIES (Helpers)
// ===============================

fn is_version_newer(current: &str, new: &str) -> bool {
    let parse_version = |v: &str| -> Vec<u32> {
        v.trim_start_matches('v')
            .split('.')
            .map(|s| s.parse::<u32>().unwrap_or(0))
            .collect()
    };

    let curr_parts = parse_version(current);
    let new_parts = parse_version(new);

    for i in 0..std::cmp::max(curr_parts.len(), new_parts.len()) {
        let c = *curr_parts.get(i).unwrap_or(&0);
        let n = *new_parts.get(i).unwrap_or(&0);
        if n > c { return true; }
        if n < c { return false; }
    }
    false
}

#[allow(unused_must_use)]
fn get_window_icon(hwnd: HWND, _ctx: &egui::Context) -> Option<egui::TextureHandle> {
    unsafe {
        let mut hicon = SendMessageW(hwnd, WM_GETICON, WPARAM(ICON_BIG as usize), LPARAM(0)).0 as isize;
        if hicon == 0 { hicon = SendMessageW(hwnd, WM_GETICON, WPARAM(ICON_SMALL as usize), LPARAM(0)).0 as isize; }
        if hicon == 0 { hicon = GetClassLongPtrW(hwnd, GCL_HICON) as isize; }
        if hicon == 0 { hicon = LoadIconW(None, IDI_APPLICATION).unwrap().0 as isize; }
        
        if hicon == 0 { return None; }
        let hicon = HICON(hicon);

        let width = 32;
        let height = 32;

        let hdc_screen = GetDC(HWND(0));
        let hdc_mem = CreateCompatibleDC(hdc_screen);
        
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height, 
                biPlanes: 1,
                biBitCount: 32, 
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut p_bits: *mut c_void = std::ptr::null_mut();
        let hbitmap = CreateDIBSection(hdc_mem, &bmi, DIB_RGB_COLORS, &mut p_bits, HANDLE(0), 0);

        if hbitmap.is_err() || p_bits.is_null() {
            DeleteDC(hdc_mem);
            ReleaseDC(HWND(0), hdc_screen);
            return None;
        }
        let hbitmap = hbitmap.unwrap();

        let old_obj = SelectObject(hdc_mem, HGDIOBJ(hbitmap.0));
        let _ = DrawIconEx(hdc_mem, 0, 0, hicon, width, height, 0, HBRUSH(0), DI_NORMAL);

        let pixel_count = (width * height) as usize;
        let src_slice = std::slice::from_raw_parts_mut(p_bits as *mut u8, pixel_count * 4);
        
        for chunk in src_slice.chunks_exact_mut(4) {
            let b = chunk[0];
            let r = chunk[2];
            chunk[0] = r; 
            chunk[2] = b; 
            if chunk[3] == 0 && (chunk[0] != 0 || chunk[1] != 0 || chunk[2] != 0) {
                chunk[3] = 255;
            }
        }

        let image = egui::ColorImage::from_rgba_unmultiplied([width as usize, height as usize], src_slice);

        SelectObject(hdc_mem, old_obj);
        DeleteObject(HGDIOBJ(hbitmap.0));
        DeleteDC(hdc_mem);
        ReleaseDC(HWND(0), hdc_screen);

        Some(_ctx.load_texture(format!("icon_{}", hicon.0), image, egui::TextureOptions::LINEAR))
    }
}


fn launch_cli() -> Result<(), String> {
    let exe_path = std::env::current_exe()
        .map_err(|e| format!("Failed to get current exe path: {}", e))?;
    
    let exe_dir = exe_path.parent()
        .ok_or("Failed to get exe directory")?;
    
    let cli_path = exe_dir.join("winhider-cli.exe");
    
    if !cli_path.exists() {
        return Err("winhider-cli.exe not found in the same directory".to_string());
    }
    
    std::process::Command::new(&cli_path)
        .spawn()
        .map_err(|e| format!("Failed to launch CLI: {}", e))?;
    
    Ok(())
}

fn truncate_middle(text: &str, max_len: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= max_len {
        return text.to_string();
    }
    let separator = "...";
    let chars_to_keep = max_len.saturating_sub(3);
    let head_len = (chars_to_keep as f32 / 2.0).ceil() as usize;
    let tail_len = (chars_to_keep as f32 / 2.0).floor() as usize;
    
    let head: String = text.chars().take(head_len).collect();
    let tail: String = text.chars().skip(char_count - tail_len).collect();
    format!("{}{}{}", head, separator, tail)
}

// Returns (eframe::IconData, egui::ColorImage)
// Returns (eframe::IconData, egui::ColorImage)
pub fn load_app_icon() -> (egui::IconData, egui::ColorImage) {
    // 1. EMBED THE ICON AT COMPILE TIME
    // This will still fail TO COMPILE if the file is missing.
    let icon_bytes = include_bytes!("../../Misc/whicon-small.ico");

    // 2. Try to decode the embedded image
    let image_result = image::load_from_memory(icon_bytes);

    match image_result {
        Ok(image) => {
            let rgba = image.to_rgba8().into_raw();
            
            let icon_data = egui::IconData {
                rgba: rgba.clone(),
                width: image.width(),
                height: image.height(),
            };
            
            let color_image = egui::ColorImage::from_rgba_unmultiplied(
                [image.width() as _, image.height() as _],
                &rgba,
            );

            (icon_data, color_image)
        }
        Err(e) => {
            eprintln!("⚠ Failed to decode embedded icon: {}. Using fallback.", e);
            
            // --- FALLBACK: Generate a 32x32 Blue Square ---
            let width = 32;
            let height = 32;
            // RGBA: Red=0, Green=0, Blue=255, Alpha=255
            let rgba = vec![0, 0, 255, 255].repeat((width * height) as usize);
            
            let icon_data = egui::IconData {
                rgba: rgba.clone(),
                width,
                height,
            };
            
            let color_image = egui::ColorImage::from_rgba_unmultiplied(
                [width as _, height as _],
                &rgba,
            );
            
            (icon_data, color_image)
        }
    }
}

fn set_capture_self(hwnd: HWND, hide: bool) -> std::result::Result<(), String> {
    unsafe {
        let affinity = if hide { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE };
        SetWindowDisplayAffinity(hwnd, affinity).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn get_eframe_hwnd(frame: &eframe::Frame) -> HWND {
    match frame.window_handle().unwrap().as_raw() {
        RawWindowHandle::Win32(handle) => HWND(handle.hwnd.get() as isize),
        _ => HWND(0),
    }
}

// In your win32 module
pub fn enumerate_windows(ctx: &egui::Context) -> Vec<AppWindow> {
    let list = Vec::new();
    let mut params = (list, ctx.clone());

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        unsafe {
            if !IsWindowVisible(hwnd).as_bool() { return BOOL(1); }

            let mut title_buf = [0u16; 256];
            let len = GetWindowTextW(hwnd, &mut title_buf);
            if len == 0 { return BOOL(1); }

            let title = String::from_utf16_lossy(&title_buf[..len as usize]);
            if winhider_core::IGNORED_WINDOWS.contains(&title.as_str()) { return BOOL(1); }

            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));

            let (list, ctx) = &mut *(lparam.0 as *mut (Vec<AppWindow>, egui::Context));

            // Detect initial topmost state
            let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            let is_topmost = (ex_style & WS_EX_TOPMOST.0) != 0;

            list.push(AppWindow {
                hwnd,
                pid,
                title,
                is_taskbar_hidden: false,
                is_capture_hidden: false,
                is_always_on_top: is_topmost,
                icon_texture: get_window_icon(hwnd, ctx)
            });
            BOOL(1)
        }
    }

    unsafe { let _ = EnumWindows(Some(enum_proc), LPARAM(&mut params as *mut _ as isize)); }
    params.0
}

fn get_config_dir() -> PathBuf {
    let mut config_dir = env::var("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(".").join("config"));
    config_dir.push(APP_NAME);
    if !config_dir.exists() {
        std::fs::create_dir_all(&config_dir).ok();
    }
    config_dir
}

fn load_auto_hide_list() -> Vec<String> {
    let config_dir = get_config_dir();
    let file_path = config_dir.join("autohide.txt");
    match std::fs::read_to_string(file_path) {
        Ok(content) => content
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn save_auto_hide_list(list: &[String]) -> std::io::Result<()> {
    let config_dir = get_config_dir();
    let file_path = config_dir.join("autohide.txt");
    let content = list.join("\n");
    std::fs::write(file_path, content)
}

fn load_settings() -> AppSettings {
    let config_dir = get_config_dir();
    let file_path = config_dir.join("settings.json");
    match std::fs::read_to_string(file_path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_else(|_| AppSettings {
            enable_auto_update: true,
            preview_quality: 2,
            global_hotkey_enabled: true,
            global_hotkey_vk: 0xC0,
        }),
        Err(_) => AppSettings {
            enable_auto_update: true,
            preview_quality: 2,
            global_hotkey_enabled: true,
            global_hotkey_vk: 0xC0,
        },
    }
}

fn save_settings(settings: &AppSettings) -> std::io::Result<()> {
    let config_dir = get_config_dir();
    let file_path = config_dir.join("settings.json");
    let content = serde_json::to_string_pretty(settings).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(file_path, content)
}

impl WinHiderApp {
    fn current_settings(&self) -> AppSettings {
        AppSettings {
            enable_auto_update: self.enable_auto_update,
            preview_quality: self.preview_quality,
            global_hotkey_enabled: self.global_hotkey_enabled,
            global_hotkey_vk: self.global_hotkey_vk,
        }
    }

    fn should_auto_hide(&self, window_title: &str) -> bool {
        for app_name in &self.auto_hide_list {
            if !app_name.is_empty() && window_title.to_lowercase().contains(&app_name.to_lowercase()) {
                return true;
            }
        }
        false
    }

    fn update_stealth_focus(&mut self, self_hwnd: HWND) {
        let fg_hwnd = unsafe { GetForegroundWindow() };
        if fg_hwnd.0 == 0 {
            return;
        }

        // Determine if the foreground window is "hidden" (capture-hidden)
        let fg_is_hidden = if fg_hwnd == self_hwnd {
            // WinHider itself counts as hidden only if self-hide-capture is on
            self.self_hide_capture
        } else {
            self.windows.iter().any(|w| w.hwnd == fg_hwnd && w.is_capture_hidden)
        };

        if fg_is_hidden {
            // A hidden window is foreground — activate stealth
            let sf = &mut self.stealth_focus;

            // Guard: cover must exist and still be a valid window
            if sf.cover_hwnd.0 == 0 || !unsafe { IsWindow(sf.cover_hwnd) }.as_bool() {
                sf.active = false;
                sf.cover_hwnd = HWND(0);
                return;
            }

            if !sf.active {
                sf.active = true;
                sf.heartbeat_counter = 0;
                Self::send_stealth_overrides(sf);
            } else {
                sf.heartbeat_counter += 1;
                if sf.heartbeat_counter >= 30 {
                    sf.heartbeat_counter = 0;
                    Self::send_stealth_overrides(sf);
                }
            }
        } else {
            // A normal visible window is foreground — update cover, deactivate stealth
            if fg_hwnd != self_hwnd {
                self.stealth_focus.cover_hwnd = fg_hwnd;
            }
            self.stealth_focus.active = false;
        }
    }

    fn send_stealth_overrides(sf: &mut StealthFocusState) {
        let cover = sf.cover_hwnd;
        unsafe {
            // Force cover window's title bar to draw as "active"
            SendMessageW(cover, WM_NCACTIVATE, WPARAM(1), LPARAM(0));
        }
        // Force cover window's taskbar button to appear highlighted
        if let Some(tbl) = sf.get_taskbar_list() {
            unsafe {
                let _ = tbl.ActivateTab(cover);
            }
        }
    }
}

fn main() -> eframe::Result<()> {
    // Initialize COM for ITaskbarList (stealth focus)
    unsafe { let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED); }

    // Load Icon data for Window Titlebar (Reuse logic via utils)
    // This now safely handles the ICO file without crashing
    let (icon_data, _) = load_app_icon();

    eframe::run_native(
        "WinHider",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([500.0, 700.0])
                .with_min_inner_size([495.0, 600.0])
                .with_max_inner_size([1600.0, 1800.0])
                .with_icon(icon_data), // Set Window Icon
            ..Default::default()
        },
        Box::new(|cc| {
            egui_extras::install_image_loaders(&cc.egui_ctx);
            // 2. Initialize App (Loads Internal Texture)
            Box::new(WinHiderApp::new(cc))
        }),
    )
}