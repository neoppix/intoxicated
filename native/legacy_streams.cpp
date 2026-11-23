// bionic's pre-API-23 `__sF` streams, for the stdio calls the engine makes
// with them.
//
// The engine imports `__sF`, bionic's array of three FILE slots (stdin,
// stdout, stderr) that its old `stdin`/`stdout`/`stderr` macros pointed into.
// The runtime gives it zeroed storage for that array
// (`crates/intoxicated-runtime/src/bionic/overrides.rs`: `intoxicated_legacy_sf_base`,
// 3 x `intoxicated_legacy_sf_stride` = 3 x 152 bytes, bionic's LP64 FILE). A
// host stdio call handed a pointer into that storage would read the zeros as
// a FreeBSD FILE. So each FILE-taking import the engine has is installed as one
// of these, which swaps a pointer into slot k (anywhere inside it) for the
// host's stdin, stdout or stderr and passes every other pointer, null
// included, through unchanged, then calls the host function with the same
// other arguments. Nothing else is interpreted, and no FILE is emulated.
//
// Wrapped: fflush, fclose, fseek, ftell, fputs, setvbuf, fread, fwrite,
// fprintf, vfprintf, fputc, fgets, ferror, feof and fileno, all imported by
// libroblox.so. It also imports clearerr, fputwc, fscanf, fseeko, ftello, getc
// and ungetc, which take a FILE too and are not wrapped; no call with an
// `__sF` pointer has been seen through those or through the last five above.
// The flag and whence constants (_IOFBF/_IOLBF/_IONBF, SEEK_*) agree between
// bionic and FreeBSD.

#include <cstdarg>
#include <cstddef>
#include <cstdint>
#include <cstdio>

extern "C" const unsigned char* intoxicated_legacy_sf_base(void);
extern "C" size_t intoxicated_legacy_sf_stride(void);

namespace {

/// The host stream for a pointer into `__sF`, or `f` itself.
FILE* host(FILE* f) {
    const unsigned char* base = intoxicated_legacy_sf_base();
    if (base == nullptr || f == nullptr) {
        return f;
    }
    const size_t stride = intoxicated_legacy_sf_stride();
    const uintptr_t p = reinterpret_cast<uintptr_t>(f);
    const uintptr_t b = reinterpret_cast<uintptr_t>(base);
    if (stride == 0 || p < b || p - b >= 3 * stride) {
        return f;
    }
    switch ((p - b) / stride) {
    case 0:
        return stdin;
    case 1:
        return stdout;
    default:
        return stderr;
    }
}

} // namespace

extern "C" {

int intoxicated_legacy_fflush(FILE* f) {
    return fflush(host(f));
}

int intoxicated_legacy_fclose(FILE* f) {
    return fclose(host(f));
}

int intoxicated_legacy_fseek(FILE* f, long offset, int whence) {
    return fseek(host(f), offset, whence);
}

long intoxicated_legacy_ftell(FILE* f) {
    return ftell(host(f));
}

int intoxicated_legacy_fputs(const char* s, FILE* f) {
    return fputs(s, host(f));
}

int intoxicated_legacy_setvbuf(FILE* f, char* buf, int mode, size_t size) {
    return setvbuf(host(f), buf, mode, size);
}

size_t intoxicated_legacy_fread(void* ptr, size_t size, size_t n, FILE* f) {
    return fread(ptr, size, n, host(f));
}

size_t intoxicated_legacy_fwrite(const void* ptr, size_t size, size_t n, FILE* f) {
    return fwrite(ptr, size, n, host(f));
}

int intoxicated_legacy_vfprintf(FILE* f, const char* format, va_list ap) {
    return vfprintf(host(f), format, ap);
}

int intoxicated_legacy_fprintf(FILE* f, const char* format, ...) {
    va_list ap;
    va_start(ap, format);
    const int r = vfprintf(host(f), format, ap);
    va_end(ap);
    return r;
}

int intoxicated_legacy_fputc(int c, FILE* f) {
    return fputc(c, host(f));
}

char* intoxicated_legacy_fgets(char* s, int n, FILE* f) {
    return fgets(s, n, host(f));
}

int intoxicated_legacy_ferror(FILE* f) {
    return ferror(host(f));
}

int intoxicated_legacy_feof(FILE* f) {
    return feof(host(f));
}

int intoxicated_legacy_fileno(FILE* f) {
    return fileno(host(f));
}

} // extern "C"
