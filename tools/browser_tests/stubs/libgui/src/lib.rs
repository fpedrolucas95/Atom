//! Host stub of Atom's `libgui`: just the `Color` type the engine modules use.

pub mod color {
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct Color {
        pub r: u8,
        pub g: u8,
        pub b: u8,
    }

    impl Color {
        pub const WHITE: Color = Color::rgb(255, 255, 255);

        pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
            Self { r, g, b }
        }
    }
}
