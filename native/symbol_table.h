// symbol_table.h: the entry type of the `intoxicated_*_symbols(size_t* count)`
// tables this directory hands the Rust side.
//
// `crates/intoxicated-runtime/src/bionic/mod.rs` reads each table as an array of
// (name, address) pairs, treats a null table as empty, skips entries with a
// null name and installs the rest into the engine's import resolution, a
// later table replacing an earlier one by name. So a table has static
// lifetime and its names are string literals.
#pragma once

#include <cstddef>

struct IntoxicatedSymbol {
    const char* name;
    void* addr;
};

/// The element count of a static table, for the `count` out-parameter.
template <class T, size_t N>
constexpr size_t intoxicated_table_size(const T (&)[N]) {
    return N;
}
