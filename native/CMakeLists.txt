# Intoxicated's native layer: the bionic linker port, libjnivm and the
# Android framework shims written for this project.
#
# `crates/intoxicated-linker-sys/build.rs` configures this directory with
# clang and clang++ in the Release configuration, runs the install step and
# links, from <prefix>/lib and in this order, the six archives installed at
# the end of this file: intoxicated_linker_shim, intoxicated_jni_shim,
# intoxicated_liblog, jnivm, logger, linker. Every object goes into a PIE
# Rust binary, so everything is built position-independent.
cmake_minimum_required(VERSION 3.16)
project(intoxicated_native LANGUAGES C CXX)

# The bionic sources use Clang extensions. FreeBSD's base compiler is Clang,
# so this only stops a deliberate override.
if(NOT CMAKE_CXX_COMPILER_ID MATCHES "Clang")
  message(FATAL_ERROR "native/ needs Clang; CMAKE_CXX_COMPILER is ${CMAKE_CXX_COMPILER_ID} (${CMAKE_CXX_COMPILER})")
endif()

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

# libjnivm's per-call JNI trace, which also prints every
# `Constructed Unresolved symbol` the gap register counts. build.rs turns it on
# when INTOXICATED_JNI_TRACE is in cargo's environment. Very slow.
option(INTOXICATED_JNI_TRACE "Build libjnivm with its JNI call trace" OFF)

# ------------------------------------------------------------ the bionic linker

add_subdirectory(${CMAKE_CURRENT_SOURCE_DIR}/../third_party/mcpelauncher-linker
                 ${CMAKE_CURRENT_BINARY_DIR}/mcpelauncher-linker)

# Upstream compiles the linker with PATH_MAX=256. Seven char[PATH_MAX] buffers
# in it receive host paths, and FreeBSD's own PATH_MAX is 1024
# (sys/syslimits.h), so 256 is too small for a real path. The definition
# itself has to stay: those files do not see a <limits.h> that defines it.
# Corrected here rather than in third_party/, which is not edited.
get_target_property(_linker_definitions linker COMPILE_DEFINITIONS)
list(TRANSFORM _linker_definitions REPLACE "^PATH_MAX=.*$" "PATH_MAX=4096")
set_property(TARGET linker PROPERTY COMPILE_DEFINITIONS ${_linker_definitions})

# ------------------------------------------------------------ libjnivm

# libjnivm's own CMakeLists links a target named `logger` when one already
# exists, and then defines HAVE_LOGGER, which routes its log calls to
# jnivm_logger/log.h. So this comes first, and the name is libjnivm's.
add_library(logger STATIC jnivm_logger/jnivm_log.cpp)
target_include_directories(logger PUBLIC ${CMAKE_CURRENT_SOURCE_DIR}/jnivm_logger)

# JNIVM_ENABLE_DEBUG defines JNI_DEBUG publicly and builds the class dump
# (`intoxicated_jni_dump_classes`); every file that includes jnivm.h has to see
# the same value, which the public definition guarantees.
set(JNIVM_ENABLE_DEBUG ON CACHE BOOL "" FORCE)
set(JNIVM_ENABLE_TRACE ${INTOXICATED_JNI_TRACE} CACHE BOOL "" FORCE)
set(JNIVM_ENABLE_TESTS OFF CACHE BOOL "" FORCE)
set(JNIVM_BUILD_EXAMPLES OFF CACHE BOOL "" FORCE)
# Excluded from ALL so its fake-jni and baron targets are not built; jnivm
# itself is, because intoxicated_jni_shim links it.
add_subdirectory(${CMAKE_CURRENT_SOURCE_DIR}/../third_party/libjnivm
                 ${CMAKE_CURRENT_BINARY_DIR}/libjnivm EXCLUDE_FROM_ALL)

# ------------------------------------------------------------ our archives

# The libc layer the engine is linked against, and the audio devices.
#
# `freebsd_abi.c` sits in this archive rather than beside freebsd_libc_compat.c
# because system_paths.cpp and netdb_linux.cpp call into it: the Linux flag,
# sockaddr and errno translation the engine needs on FreeBSD. It compiles to
# nothing anywhere else.
#
# Audio is FreeBSD sound(4) only: `oss_device.cpp` opens and negotiates
# /dev/dsp*, `aaudio_oss.cpp` is the engine's libaaudio.so over it and
# `opensl_refusal.cpp` the link-time libOpenSLES.so symbols. Nothing is found,
# linked or dlopen'd for any of it: <sys/soundcard.h> is part of base.
add_library(intoxicated_liblog STATIC
  android_log.cpp system_paths.cpp netdb_linux.cpp oss_device.cpp aaudio_oss.cpp
  opensl_refusal.cpp thread_create.cpp freebsd_abi.c)

# The /proc/self/maps rewrite system_paths.cpp serves the engine, checked on
# every build, because a check that only runs when asked for silently stops
# running. A wrong rewrite
# produces a plausible map and nothing fails until the anticheat reads it, so
# this pins the launcher layout's output byte for byte and checks that another
# HOME, APK and executable produce the same map.
add_executable(intoxicated_maps_rewrite_test EXCLUDE_FROM_ALL maps_rewrite_test.cpp)
add_custom_command(TARGET intoxicated_maps_rewrite_test POST_BUILD
  COMMAND intoxicated_maps_rewrite_test
  COMMENT "Running the /proc/self/maps rewrite checks")
add_dependencies(intoxicated_liblog intoxicated_maps_rewrite_test)

# The C surface over the linker's C++ API, and the libc pieces that sit
# beside it.
add_library(intoxicated_linker_shim STATIC linker_surface.cpp freebsd_libc_compat.c fortify_chk.c syscall_trap.c)
target_link_libraries(intoxicated_linker_shim PUBLIC linker)

# `fmod_java.cpp` is the org.fmod Java side; it calls into oss_device.cpp in
# intoxicated_liblog for the device probe and the AudioDevice fallback.
#
# The Java world the engine sees: `jni_framework.cpp`, `activity.cpp` and
# `jni_params.cpp` register its classes, `jni_world.cpp` holds what they
# share, and `jni_values.cpp` the values and state behind them, with no
# libjnivm in it so `jni_values_test.cpp` can check it on every build.
add_library(intoxicated_jni_shim STATIC java_vm.cpp jni_values.cpp jni_world.cpp jni_framework.cpp activity.cpp jni_params.cpp jni_cookies.cpp fmod_java.cpp deeplink.cpp message_bus.cpp secure_values.cpp legacy_streams.cpp android_platform.cpp battery_status.cpp)
target_link_libraries(intoxicated_jni_shim PUBLIC jnivm)

add_executable(intoxicated_jni_values_test EXCLUDE_FROM_ALL jni_values_test.cpp jni_values.cpp)
add_custom_command(TARGET intoxicated_jni_values_test POST_BUILD
  COMMAND intoxicated_jni_values_test
  COMMENT "Running the Java-world value checks")
add_dependencies(intoxicated_jni_shim intoxicated_jni_values_test)

# The files these archives replaced clean-room, checked through the archives
# themselves on every build: the netdb translation against FreeBSD's own
# getaddrinfo, the liblog line format, the __sF stream wrappers, the thread
# trace's libroblox.so lookup, the cookie and deep-link JNI calls and the
# platform classes. It links what the Rust binary links, with stand-ins for
# the three Rust symbols the archives call.
add_executable(intoxicated_small_native_test small_native_test.cpp)
target_link_libraries(intoxicated_small_native_test PRIVATE
  intoxicated_linker_shim intoxicated_jni_shim intoxicated_liblog jnivm logger linker execinfo)
add_custom_command(TARGET intoxicated_small_native_test POST_BUILD
  COMMAND intoxicated_small_native_test
  COMMENT "Running the small native file checks")

install(TARGETS intoxicated_linker_shim intoxicated_jni_shim intoxicated_liblog jnivm logger linker
  ARCHIVE DESTINATION lib)
