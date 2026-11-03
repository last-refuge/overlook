# Overlook color icons

Original vector artwork created for Overlook. No external icon pack or logo assets are used.

The 32-unit drawings use rounded strokes, soft fills and restrained two-tone accents. Their 128-pixel intrinsic size keeps them crisp on HiDPI screens. Render with GPUI `img` to retain RGB gradients, rather than the monochrome icon mask.

The source files preview the Light theme. `assets.rs` replaces six color tokens from the active `Palette`: primary `#3F6654`, highlight `#76A58C`, secondary `#92581D`, tint `#DDE9E1`, paper `#FFFFFF`, ink `#29332E`. Each resolved theme and normal/on-action contrast context has a distinct resource path, so switching appearance refreshes the artwork immediately without stale image-cache colors.
