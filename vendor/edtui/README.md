# Vendored edtui

This is `edtui` 0.11.6, licensed under MIT and sourced from
<https://github.com/preiter93/edtui>.

CST vendors it because upstream's visual wrapping splits at character boundaries.
The local patch wraps at whitespace while keeping rendering, cursor placement,
viewport calculations, and mouse hit-testing consistent.
