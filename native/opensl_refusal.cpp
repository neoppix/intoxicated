// libOpenSLES.so, reduced to what the engine links against.
//
// libroblox.so references eight OpenSL ES names directly: slCreateEngine and
// seven SL_IID_* data symbols. The data symbols must resolve when the library
// is loaded or the load fails outright, so they exist here as distinct,
// non-null interface ids. The engine never calls slCreateEngine on any
// measured run (FMOD reaches AAudio, or org.fmod.AudioDevice when AAudio is
// unavailable), so the engine entry point answers
// SL_RESULT_FEATURE_UNSUPPORTED instead of handing back an object that cannot
// play anything.
//
// The id contents are local: no object is ever created here, so no caller can
// compare an id against an interface this library implements. Each one only
// has to be a different address with a different value.
#include <cstddef>
#include <cstdint>
#include <cstdio>

namespace {

struct SLInterfaceID_ {
    uint32_t time_low;
    uint16_t time_mid;
    uint16_t time_hi_and_version;
    uint16_t clock_seq;
    uint8_t node[6];
};
using SLInterfaceID = const SLInterfaceID_*;

// 'I','X' in the node bytes marks these as this library's own.
constexpr SLInterfaceID_ kEngine = {1, 0, 0, 0, {'I', 'X', 0, 0, 0, 1}};
constexpr SLInterfaceID_ kPlay = {2, 0, 0, 0, {'I', 'X', 0, 0, 0, 2}};
constexpr SLInterfaceID_ kRecord = {3, 0, 0, 0, {'I', 'X', 0, 0, 0, 3}};
constexpr SLInterfaceID_ kVolume = {4, 0, 0, 0, {'I', 'X', 0, 0, 0, 4}};
constexpr SLInterfaceID_ kBufferQueue = {5, 0, 0, 0, {'I', 'X', 0, 0, 0, 5}};
constexpr SLInterfaceID_ kAndroidSimpleBufferQueue = {6, 0, 0, 0, {'I', 'X', 0, 0, 0, 6}};
constexpr SLInterfaceID_ kAndroidConfiguration = {7, 0, 0, 0, {'I', 'X', 0, 0, 0, 7}};

// The guest resolves `SL_IID_ENGINE` to the address of a pointer variable,
// exactly as a C `extern const SLInterfaceID SL_IID_ENGINE;` would be laid out.
SLInterfaceID g_iid_engine = &kEngine;
SLInterfaceID g_iid_play = &kPlay;
SLInterfaceID g_iid_record = &kRecord;
SLInterfaceID g_iid_volume = &kVolume;
SLInterfaceID g_iid_bufferqueue = &kBufferQueue;
SLInterfaceID g_iid_android_simple_bufferqueue = &kAndroidSimpleBufferQueue;
SLInterfaceID g_iid_android_configuration = &kAndroidConfiguration;

constexpr uint32_t SL_RESULT_FEATURE_UNSUPPORTED = 0x0000000C; // OpenSL ES 1.0.1

extern "C" uint32_t ix_slCreateEngine(void** engine, uint32_t, const void*, uint32_t, const void*, const void*) {
    static bool said = false;
    if (!said) {
        said = true;
        fprintf(stderr, "[audio] slCreateEngine called: OpenSL ES is not provided -> SL_RESULT_FEATURE_UNSUPPORTED\n");
    }
    if (engine) *engine = nullptr;
    return SL_RESULT_FEATURE_UNSUPPORTED;
}

struct SymbolEntry {
    const char* name;
    void* addr;
};

const SymbolEntry kSymbols[] = {
    {"slCreateEngine", reinterpret_cast<void*>(&ix_slCreateEngine)},
    {"SL_IID_ENGINE", &g_iid_engine},
    {"SL_IID_PLAY", &g_iid_play},
    {"SL_IID_RECORD", &g_iid_record},
    {"SL_IID_VOLUME", &g_iid_volume},
    {"SL_IID_BUFFERQUEUE", &g_iid_bufferqueue},
    {"SL_IID_ANDROIDSIMPLEBUFFERQUEUE", &g_iid_android_simple_bufferqueue},
    {"SL_IID_ANDROIDCONFIGURATION", &g_iid_android_configuration},
};

} // namespace

extern "C" const SymbolEntry* intoxicated_opensl_symbols(size_t* count) {
    *count = sizeof(kSymbols) / sizeof(kSymbols[0]);
    return kSymbols;
}
