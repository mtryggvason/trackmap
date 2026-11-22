//! Postman-like dark theme: neutral greys with a single orange accent.

use gpui::{rgb, App, Global, Rgba};

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub bg_app: Rgba,
    pub bg_panel: Rgba,
    pub bg_elevated: Rgba,
    pub border: Rgba,
    pub text: Rgba,
    pub text_muted: Rgba,
    pub accent: Rgba,
    pub accent_hover: Rgba,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            bg_app: rgb(0x1C1C1C),
            bg_panel: rgb(0x212121),
            bg_elevated: rgb(0x2B2B2B),
            border: rgb(0x363636),
            text: rgb(0xE6E6E6),
            text_muted: rgb(0x9B9B9B),
            accent: rgb(0xFF6C37),
            accent_hover: rgb(0xFF8A5C),
        }
    }

    pub fn get(cx: &App) -> &Self {
        cx.global::<Self>()
    }
}

impl Global for Theme {}
