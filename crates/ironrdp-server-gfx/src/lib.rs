#![cfg_attr(doc, doc = include_str!("../README.md"))]
#![doc(
    html_logo_url = "https://cdnweb.devolutions.net/images/projects/devolutions/logos/devolutions-icon-shadow.svg"
)]
#![allow(clippy::arithmetic_side_effects)]
// TODO: should we enable this lint back?
// Upstream ironrdp code uses `as` casts extensively for wire-format protocol fields.
// Suppressing these lints avoids churn in patched upstream code.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    clippy::as_conversions,
    clippy::mixed_attributes_style,
    clippy::unwrap_used,
    clippy::missing_panics_doc,
    clippy::if_then_some_else_none,
    clippy::fn_params_excessive_bools,
    clippy::too_many_arguments,
    clippy::struct_field_names,
    clippy::partial_pub_fields,
    clippy::await_holding_lock,
    clippy::blocks_in_conditions,
    clippy::useless_let_if_seq
)]

pub use {tokio, tokio_rustls};

mod macros;

mod builder;
mod capabilities;
mod clipboard;
mod display;
mod encoder;
pub mod gfx;
mod handler;
#[cfg(feature = "helper")]
mod helper;
mod server;
mod sound;

pub use clipboard::*;
pub use display::*;
pub use handler::*;
#[cfg(feature = "helper")]
pub use helper::*;
pub use server::*;
pub use sound::*;

#[cfg(feature = "__bench")]
pub mod bench {
    pub mod encoder {
        pub mod rfx {
            pub use crate::encoder::rfx::bench::{rfx_enc, rfx_enc_tile};
        }

        pub use crate::encoder::{UpdateEncoder, UpdateEncoderCodecs};
    }
}
