//! Search backends for lsp-bridge.
//!
//! Ports of Python's search_file_words.py, search_paths.py,
//! search_list.py, ctags.py, and search_sdcv_words.py.

pub mod file_words;
pub mod paths;
pub mod list;
pub mod ctags;
