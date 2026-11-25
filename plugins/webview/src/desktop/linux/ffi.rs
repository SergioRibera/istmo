//! Hand-written C bindings for `GLib`, GTK 3, `WebKitGTK` 4.1, libsoup 3
//! and cairo, resolved with `dlopen` at run time.
//!
//! Only sonames are loaded (never a path from configuration or the
//! working directory), and the libraries are never unloaded: `GLib` type
//! registrations cannot be undone.

#![allow(
    non_camel_case_types,
    non_snake_case,
    reason = "C names kept verbatim so they match the reference documentation"
)]

use std::ffi::{c_char, c_int, c_uint, c_void};

use libloading::Library;

pub(crate) type gboolean = c_int;
pub(crate) type gpointer = *mut c_void;
pub(crate) type GType = usize;
pub(crate) type GCallback = unsafe extern "C" fn();
pub(crate) type GSourceFunc = unsafe extern "C" fn(gpointer) -> gboolean;
pub(crate) type GDestroyNotify = unsafe extern "C" fn(gpointer);
pub(crate) type GUnixFdSourceFunc = unsafe extern "C" fn(c_int, c_uint, gpointer) -> gboolean;
/// `GIOCondition` bits.
pub(crate) const G_IO_OUT: c_uint = 4;
pub(crate) const G_IO_ERR: c_uint = 8;
pub(crate) const G_IO_HUP: c_uint = 16;
pub(crate) type GClosureNotify = unsafe extern "C" fn(gpointer, gpointer);
pub(crate) type GAsyncReadyCallback = unsafe extern "C" fn(gpointer, gpointer, gpointer);
pub(crate) type WebKitURISchemeRequestCallback = unsafe extern "C" fn(gpointer, gpointer);
pub(crate) type cairo_write_func_t = unsafe extern "C" fn(gpointer, *const u8, c_uint) -> c_int;

pub(crate) const FALSE: gboolean = 0;
pub(crate) const TRUE: gboolean = 1;

/// `GValue`: a type tag and two 64-bit data words.
#[repr(C)]
#[derive(Debug)]
pub(crate) struct GValue {
    pub(crate) g_type: GType,
    pub(crate) data: [u64; 2],
}

/// `GError`.
#[repr(C)]
pub(crate) struct GError {
    pub(crate) domain: u32,
    pub(crate) code: c_int,
    pub(crate) message: *const c_char,
}

/// `GList`.
#[repr(C)]
pub(crate) struct GList {
    pub(crate) data: gpointer,
    pub(crate) next: *mut Self,
    pub(crate) prev: *mut Self,
}

/// `GdkRGBA`.
#[repr(C)]
pub(crate) struct GdkRGBA {
    pub(crate) red: f64,
    pub(crate) green: f64,
    pub(crate) blue: f64,
    pub(crate) alpha: f64,
}

macro_rules! api {
    (
        $( $lib:ident { $( fn $name:ident ( $($arg:ty),* ) $(-> $ret:ty)? ; )* } )*
        @optional {
            $( $olib:ident { $( fn $oname:ident ( $($oarg:ty),* ) $(-> $oret:ty)? ; )* } )*
        }
    ) => {
        /// Every symbol the GTK backend calls.
        pub(crate) struct Api {
            $( $( pub(crate) $name: unsafe extern "C" fn($($arg),*) $(-> $ret)?, )* )*
            /// Symbols that only one `WebKitGTK` / GTK version has.
            $( $( pub(crate) $oname: Option<unsafe extern "C" fn($($oarg),*) $(-> $oret)?>, )* )*
            _libs: Vec<Library>,
        }

        impl Api {
            /// Resolve every symbol from `libs` (sonames), in order.
            pub(crate) fn load(libs: &Libs) -> Result<Self, String> {
                Ok(Self {
                    $( $(
                        // SAFETY: the declared signature matches the C
                        // prototype of the symbol in the library it is
                        // looked up in; the library outlives the pointer
                        // because `Api` keeps it (and is never dropped
                        // while the UI thread runs).
                        $name: unsafe {
                            *libs.$lib.get::<unsafe extern "C" fn($($arg),*) $(-> $ret)?>(
                                concat!(stringify!($name), "\0").as_bytes(),
                            )
                            .map_err(|e| format!("{}: {e}", stringify!($name)))?
                        },
                    )* )*
                    $( $(
                        // SAFETY: as above; a missing symbol is `None`.
                        $oname: unsafe {
                            libs.$olib
                                .get::<unsafe extern "C" fn($($oarg),*) $(-> $oret)?>(
                                    concat!(stringify!($oname), "\0").as_bytes(),
                                )
                                .ok()
                                .map(|s| *s)
                        },
                    )* )*
                    _libs: Vec::new(),
                })
            }
        }
    };
}

/// The opened libraries.
pub(crate) struct Libs {
    pub(crate) glib: Library,
    pub(crate) gobject: Library,
    pub(crate) gio: Library,
    pub(crate) gtk: Library,
    pub(crate) gdk: Library,
    pub(crate) webkit: Library,
    pub(crate) jsc: Library,
    pub(crate) soup: Library,
    pub(crate) cairo: Library,
}

impl Libs {
    /// Open the `WebKitGTK` 4.1 (GTK 3) stack by soname.
    pub(crate) fn open_webkit2gtk_41() -> Result<Self, String> {
        Self::open(&[
            "libgtk-3.so.0",
            "libgdk-3.so.0",
            "libwebkit2gtk-4.1.so.0",
            "libjavascriptcoregtk-4.1.so.0",
        ])
    }

    /// Open the `WebKitGTK` 6.0 (GTK 4) stack by soname. GDK is part of
    /// `libgtk-4`.
    pub(crate) fn open_webkitgtk_60() -> Result<Self, String> {
        Self::open(&[
            "libgtk-4.so.1",
            "libgtk-4.so.1",
            "libwebkitgtk-6.0.so.4",
            "libjavascriptcoregtk-6.0.so.1",
        ])
    }

    fn open(toolkit: &[&str; 4]) -> Result<Self, String> {
        let open = |name: &str| {
            // SAFETY: loading a system library runs its constructors;
            // these are the GNOME stack's own libraries, loaded by
            // soname through the dynamic linker's normal search path.
            unsafe { Library::new(name) }.map_err(|e| format!("{name}: {e}"))
        };
        Ok(Self {
            glib: open("libglib-2.0.so.0")?,
            gobject: open("libgobject-2.0.so.0")?,
            gio: open("libgio-2.0.so.0")?,
            gtk: open(toolkit[0])?,
            gdk: open(toolkit[1])?,
            webkit: open(toolkit[2])?,
            jsc: open(toolkit[3])?,
            soup: open("libsoup-3.0.so.0")?,
            cairo: open("libcairo.so.2")?,
        })
    }
}

api! {
    glib {
        fn g_main_context_default() -> gpointer;
        fn g_main_context_acquire(gpointer) -> gboolean;
        fn g_main_loop_new(gpointer, gboolean) -> gpointer;
        fn g_main_loop_run(gpointer);
        fn g_idle_add_full(c_int, GSourceFunc, gpointer, Option<GDestroyNotify>) -> c_uint;
        fn g_timeout_add_full(c_int, c_uint, GSourceFunc, gpointer, Option<GDestroyNotify>) -> c_uint;
        fn g_unix_fd_add_full(c_int, c_int, c_uint, GUnixFdSourceFunc, gpointer, Option<GDestroyNotify>) -> c_uint;
        fn g_unix_set_fd_nonblocking(c_int, gboolean, *mut *mut GError) -> gboolean;
        fn g_free(gpointer);
        fn g_error_free(*mut GError);
        fn g_bytes_new(*const c_void, usize) -> gpointer;
        fn g_bytes_unref(gpointer);
        fn g_list_free(*mut GList);
        fn g_date_time_to_unix(gpointer) -> i64;
    }
    gobject {
        fn g_object_ref(gpointer) -> gpointer;
        fn g_object_unref(gpointer);
        fn g_object_new_with_properties(GType, c_uint, *const *const c_char, *const GValue) -> gpointer;
        fn g_signal_connect_data(gpointer, *const c_char, GCallback, gpointer, Option<GClosureNotify>, c_int) -> u64;
        fn g_value_init(*mut GValue, GType) -> *mut GValue;
        fn g_value_set_object(*mut GValue, gpointer);
        fn g_value_set_string(*mut GValue, *const c_char);
        fn g_value_unset(*mut GValue);
        fn g_type_check_instance_is_a(gpointer, GType) -> gboolean;
    }
    gio {
        fn g_memory_input_stream_new_from_bytes(gpointer) -> gpointer;
        fn g_unix_input_stream_new(c_int, gboolean) -> gpointer;
    }
    gtk {
        fn gtk_window_set_title(gpointer, *const c_char);
        fn gtk_window_set_default_size(gpointer, c_int, c_int);
        fn gtk_window_set_resizable(gpointer, gboolean);
        fn gtk_window_set_modal(gpointer, gboolean);
        fn gtk_window_set_decorated(gpointer, gboolean);
        fn gtk_window_present(gpointer);
        fn gtk_widget_hide(gpointer);
        fn gtk_widget_realize(gpointer);
        fn gtk_widget_grab_focus(gpointer);
    }
    gdk {
        fn gdk_set_allowed_backends(*const c_char);
        fn gdk_display_get_default() -> gpointer;
        fn gdk_x11_display_get_type() -> GType;
    }
    webkit {
        fn webkit_get_major_version() -> c_uint;
        fn webkit_get_minor_version() -> c_uint;
        fn webkit_get_micro_version() -> c_uint;
        fn webkit_web_view_get_type() -> GType;
        fn webkit_web_context_get_type() -> GType;
        fn webkit_website_data_manager_get_type() -> GType;
        fn webkit_web_context_get_default() -> gpointer;
        fn webkit_web_context_register_uri_scheme(gpointer, *const c_char, WebKitURISchemeRequestCallback, gpointer, Option<GDestroyNotify>);
        fn webkit_web_context_get_security_manager(gpointer) -> gpointer;
        fn webkit_security_manager_register_uri_scheme_as_secure(gpointer, *const c_char);
        fn webkit_security_manager_register_uri_scheme_as_cors_enabled(gpointer, *const c_char);
        fn webkit_web_view_get_context(gpointer) -> gpointer;
        fn webkit_web_view_load_uri(gpointer, *const c_char);
        fn webkit_web_view_load_html(gpointer, *const c_char, *const c_char);
        fn webkit_web_view_load_request(gpointer, gpointer);
        fn webkit_uri_request_new(*const c_char) -> gpointer;
        fn webkit_uri_request_get_uri(gpointer) -> *const c_char;
        fn webkit_uri_request_get_http_headers(gpointer) -> gpointer;
        fn webkit_web_view_go_back(gpointer);
        fn webkit_web_view_go_forward(gpointer);
        fn webkit_web_view_reload(gpointer);
        fn webkit_web_view_reload_bypass_cache(gpointer);
        fn webkit_web_view_stop_loading(gpointer);
        fn webkit_web_view_can_go_back(gpointer) -> gboolean;
        fn webkit_web_view_can_go_forward(gpointer) -> gboolean;
        fn webkit_web_view_get_uri(gpointer) -> *const c_char;
        fn webkit_web_view_get_title(gpointer) -> *const c_char;
        fn webkit_web_view_get_estimated_load_progress(gpointer) -> f64;
        fn webkit_web_view_get_back_forward_list(gpointer) -> gpointer;
        fn webkit_back_forward_list_get_nth_item(gpointer, c_int) -> gpointer;
        fn webkit_back_forward_list_get_back_list(gpointer) -> *mut GList;
        fn webkit_back_forward_list_get_forward_list(gpointer) -> *mut GList;
        fn webkit_back_forward_list_get_current_item(gpointer) -> gpointer;
        fn webkit_back_forward_list_item_get_uri(gpointer) -> *const c_char;
        fn webkit_back_forward_list_item_get_title(gpointer) -> *const c_char;
        fn webkit_web_view_go_to_back_forward_list_item(gpointer, gpointer);
        fn webkit_web_view_evaluate_javascript(gpointer, *const c_char, isize, *const c_char, *const c_char, gpointer, Option<GAsyncReadyCallback>, gpointer);
        fn webkit_web_view_evaluate_javascript_finish(gpointer, gpointer, *mut *mut GError) -> gpointer;
        fn webkit_web_view_call_async_javascript_function(gpointer, *const c_char, isize, gpointer, *const c_char, *const c_char, gpointer, Option<GAsyncReadyCallback>, gpointer);
        fn webkit_web_view_call_async_javascript_function_finish(gpointer, gpointer, *mut *mut GError) -> gpointer;
        fn webkit_web_view_get_settings(gpointer) -> gpointer;
        fn webkit_settings_set_enable_javascript(gpointer, gboolean);
        fn webkit_settings_set_enable_html5_local_storage(gpointer, gboolean);
        fn webkit_settings_set_media_playback_requires_user_gesture(gpointer, gboolean);
        fn webkit_settings_set_user_agent(gpointer, *const c_char);
        fn webkit_settings_get_user_agent(gpointer) -> *const c_char;
        fn webkit_settings_set_enable_developer_extras(gpointer, gboolean);
        fn webkit_settings_set_allow_file_access_from_file_urls(gpointer, gboolean);
        fn webkit_settings_set_allow_universal_access_from_file_urls(gpointer, gboolean);
        fn webkit_settings_set_javascript_can_open_windows_automatically(gpointer, gboolean);
        fn webkit_web_view_set_zoom_level(gpointer, f64);
        fn webkit_web_view_set_background_color(gpointer, *const GdkRGBA);
        fn webkit_web_view_get_inspector(gpointer) -> gpointer;
        fn webkit_web_inspector_show(gpointer);
        fn webkit_web_inspector_close(gpointer);
        fn webkit_web_view_get_find_controller(gpointer) -> gpointer;
        fn webkit_find_controller_search(gpointer, *const c_char, c_uint, c_uint);
        fn webkit_find_controller_search_finish(gpointer);
        fn webkit_web_view_get_snapshot(gpointer, c_int, c_int, gpointer, Option<GAsyncReadyCallback>, gpointer);
        fn webkit_web_view_get_snapshot_finish(gpointer, gpointer, *mut *mut GError) -> gpointer;
        fn webkit_print_operation_new(gpointer) -> gpointer;
        fn webkit_print_operation_run_dialog(gpointer, gpointer) -> c_int;
        fn webkit_policy_decision_use(gpointer);
        fn webkit_policy_decision_ignore(gpointer);
        fn webkit_navigation_policy_decision_get_navigation_action(gpointer) -> gpointer;
        fn webkit_navigation_action_get_request(gpointer) -> gpointer;
        fn webkit_navigation_action_is_user_gesture(gpointer) -> gboolean;
        fn webkit_permission_request_allow(gpointer);
        fn webkit_permission_request_deny(gpointer);
        fn webkit_user_media_permission_request_get_type() -> GType;
        fn webkit_geolocation_permission_request_get_type() -> GType;
        fn webkit_notification_permission_request_get_type() -> GType;
        fn webkit_user_media_permission_is_for_video_device(gpointer) -> gboolean;
        fn webkit_user_media_permission_is_for_audio_device(gpointer) -> gboolean;
        fn webkit_script_dialog_ref(gpointer) -> gpointer;
        fn webkit_script_dialog_unref(gpointer);
        fn webkit_script_dialog_get_dialog_type(gpointer) -> c_int;
        fn webkit_script_dialog_get_message(gpointer) -> *const c_char;
        fn webkit_script_dialog_prompt_get_default_text(gpointer) -> *const c_char;
        fn webkit_script_dialog_confirm_set_confirmed(gpointer, gboolean);
        fn webkit_script_dialog_prompt_set_text(gpointer, *const c_char);
        fn webkit_script_dialog_close(gpointer);
        fn webkit_authentication_request_get_host(gpointer) -> *const c_char;
        fn webkit_authentication_request_get_realm(gpointer) -> *const c_char;
        fn webkit_authentication_request_is_for_proxy(gpointer) -> gboolean;
        fn webkit_authentication_request_authenticate(gpointer, gpointer);
        fn webkit_authentication_request_cancel(gpointer);
        fn webkit_credential_new(*const c_char, *const c_char, c_int) -> gpointer;
        fn webkit_credential_free(gpointer);
        fn webkit_file_chooser_request_get_select_multiple(gpointer) -> gboolean;
        fn webkit_file_chooser_request_select_files(gpointer, *const *const c_char);
        fn webkit_file_chooser_request_cancel(gpointer);
        fn webkit_user_content_manager_new() -> gpointer;
        fn webkit_user_content_manager_add_script(gpointer, gpointer);
        fn webkit_user_content_manager_register_script_message_handler_with_reply(gpointer, *const c_char, *const c_char) -> gboolean;
        fn webkit_user_script_new(*const c_char, c_int, c_int, *const *const c_char, *const *const c_char) -> gpointer;
        fn webkit_user_script_unref(gpointer);
        fn webkit_script_message_reply_ref(gpointer) -> gpointer;
        fn webkit_script_message_reply_unref(gpointer);
        fn webkit_script_message_reply_return_value(gpointer, gpointer);
        fn webkit_script_message_reply_return_error_message(gpointer, *const c_char);
        fn webkit_uri_scheme_request_get_uri(gpointer) -> *const c_char;
        fn webkit_uri_scheme_request_get_http_method(gpointer) -> *const c_char;
        fn webkit_uri_scheme_request_get_web_view(gpointer) -> gpointer;
        fn webkit_uri_scheme_request_finish_with_response(gpointer, gpointer);
        fn webkit_uri_scheme_response_new(gpointer, i64) -> gpointer;
        fn webkit_uri_scheme_response_set_status(gpointer, c_uint, *const c_char);
        fn webkit_uri_scheme_response_set_content_type(gpointer, *const c_char);
        fn webkit_uri_scheme_response_set_http_headers(gpointer, gpointer);
        fn webkit_cookie_manager_get_cookies(gpointer, *const c_char, gpointer, Option<GAsyncReadyCallback>, gpointer);
        fn webkit_cookie_manager_get_cookies_finish(gpointer, gpointer, *mut *mut GError) -> *mut GList;
        fn webkit_cookie_manager_add_cookie(gpointer, gpointer, gpointer, Option<GAsyncReadyCallback>, gpointer);
        fn webkit_cookie_manager_add_cookie_finish(gpointer, gpointer, *mut *mut GError) -> gboolean;
        fn webkit_website_data_manager_clear(gpointer, c_int, i64, gpointer, Option<GAsyncReadyCallback>, gpointer);
    }
    jsc {
        fn jsc_value_to_json(gpointer, c_uint) -> *mut c_char;
        fn jsc_value_to_string(gpointer) -> *mut c_char;
        fn jsc_value_get_context(gpointer) -> gpointer;
        fn jsc_value_new_string(gpointer, *const c_char) -> gpointer;
    }
    soup {
        fn soup_message_headers_new(c_int) -> gpointer;
        fn soup_message_headers_append(gpointer, *const c_char, *const c_char);
        fn soup_cookie_new(*const c_char, *const c_char, *const c_char, *const c_char, c_int) -> gpointer;
        fn soup_cookie_free(gpointer);
        fn soup_cookie_get_name(gpointer) -> *const c_char;
        fn soup_cookie_get_value(gpointer) -> *const c_char;
        fn soup_cookie_get_domain(gpointer) -> *const c_char;
        fn soup_cookie_get_path(gpointer) -> *const c_char;
        fn soup_cookie_get_expires(gpointer) -> gpointer;
        fn soup_cookie_get_secure(gpointer) -> gboolean;
        fn soup_cookie_get_http_only(gpointer) -> gboolean;
        fn soup_cookie_get_same_site_policy(gpointer) -> c_int;
        fn soup_cookie_set_secure(gpointer, gboolean);
        fn soup_cookie_set_http_only(gpointer, gboolean);
        fn soup_cookie_set_same_site_policy(gpointer, c_int);
    }
    @optional {
        glib {
            fn g_bytes_get_data(gpointer, *mut usize) -> *const c_void;
        }
        gtk {
            // GTK 3
            fn gtk_container_add(gpointer, gpointer);
            fn gtk_widget_show_all(gpointer);
            fn gtk_widget_destroy(gpointer);
            fn gtk_widget_get_window(gpointer) -> gpointer;
            fn gtk_window_resize(gpointer, c_int, c_int);
            // GTK 4
            fn gtk_window_set_child(gpointer, gpointer);
            fn gtk_window_destroy(gpointer);
            fn gtk_widget_set_visible(gpointer, gboolean);
        }
        gdk {
            fn gdk_x11_window_get_xid(gpointer) -> std::ffi::c_ulong;
            fn gdk_x11_display_get_xdisplay(gpointer) -> gpointer;
            fn gdk_texture_save_to_png_bytes(gpointer) -> gpointer;
            fn gdk_texture_get_width(gpointer) -> c_int;
            fn gdk_texture_get_height(gpointer) -> c_int;
        }
        webkit {
            // WebKitGTK 4.1
            fn webkit_web_context_new_ephemeral() -> gpointer;
            fn webkit_web_context_set_sandbox_enabled(gpointer, gboolean);
            fn webkit_web_context_get_cookie_manager(gpointer) -> gpointer;
            fn webkit_web_context_get_website_data_manager(gpointer) -> gpointer;
            fn webkit_settings_set_enable_html5_database(gpointer, gboolean);
            // WebKitGTK 6.0
            fn webkit_network_session_get_default() -> gpointer;
            fn webkit_network_session_new_ephemeral() -> gpointer;
            fn webkit_network_session_new(*const c_char, *const c_char) -> gpointer;
            fn webkit_network_session_get_cookie_manager(gpointer) -> gpointer;
            fn webkit_network_session_get_website_data_manager(gpointer) -> gpointer;
            fn webkit_web_view_get_network_session(gpointer) -> gpointer;
        }
        cairo {
            fn cairo_surface_write_to_png_stream(gpointer, cairo_write_func_t, gpointer) -> c_int;
            fn cairo_surface_destroy(gpointer);
            fn cairo_image_surface_get_width(gpointer) -> c_int;
            fn cairo_image_surface_get_height(gpointer) -> c_int;
        }
    }
}

impl Api {
    /// Keep the libraries alive for as long as the function pointers.
    pub(crate) fn with_libs(mut self, libs: Libs) -> Self {
        self._libs = vec![
            libs.glib,
            libs.gobject,
            libs.gio,
            libs.gtk,
            libs.gdk,
            libs.webkit,
            libs.jsc,
            libs.soup,
            libs.cairo,
        ];
        self
    }
}

/// `G_TYPE_STRING` / `G_TYPE_OBJECT` fundamental ids.
pub(crate) const G_TYPE_STRING: GType = 16 << 2;
pub(crate) const G_TYPE_OBJECT: GType = 20 << 2;
