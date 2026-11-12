/*
 * SwiftPM C targets need at least one source file. The symbols declared
 * in the header are defined by the Rust staticlib the app links, so this
 * translation unit only makes the compiler check the header itself.
 */
#include "istmo_ffi.h"
