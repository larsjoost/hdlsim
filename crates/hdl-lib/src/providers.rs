//! Unit providers — the extension seam for library sources.
//!
//! A provider answers "do you have design unit (name, kind), and if not compiled
//! yet, where is its source?". Filesystem directories, the built-in `std`/`ieee`
//! libraries, and the incremental cache are all providers in one list.
