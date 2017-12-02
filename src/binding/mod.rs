//! A module of library bindings for sans
//! 
//! A lot of components that sans uses are written in C or C++. Binding
//! against them in a (as far as that's possible) safe manner is of importance.
//! 
//! This a lot of logic to abstract away FFI types and uncertainties are hidden
//! away in this submodule
