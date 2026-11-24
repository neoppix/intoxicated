# Intoxicated Settings: an optional Qt 6 Widgets app for Intoxicated's flags,
# client options and profiles. install.sh builds it with
#
#   cmake -S settings -B settings/build -DCMAKE_BUILD_TYPE=Release
#   cmake --build settings/build -j <ncpu>
#
# and installs settings/build/intoxicated-settings to $PREFIX/bin. Needs
# qt6-base and nothing else. `ctest --test-dir settings/build` runs the app's
# self-test against scratch directories.

cmake_minimum_required(VERSION 3.21)
project(intoxicated-settings LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)
if(NOT CMAKE_BUILD_TYPE)
  set(CMAKE_BUILD_TYPE Release)
endif()

find_package(Qt6 6.5 REQUIRED COMPONENTS Widgets)
qt_standard_project_setup()

qt_add_executable(intoxicated-settings
  src/main.cpp
  src/app.h
  src/app.cpp
  src/core.h
  src/core.cpp
  src/style.h
  src/style.cpp
  src/flags_page.cpp
  src/client_page.cpp
  src/shaders_page.cpp
  src/profiles_page.cpp
  src/other_pages.cpp
  src/selftest.cpp
)
target_link_libraries(intoxicated-settings PRIVATE Qt6::Widgets)
target_compile_options(intoxicated-settings PRIVATE -Wall -Wextra)

# The logo, compiled in, so the app finds it wherever it is installed.
set(logo "${CMAKE_CURRENT_SOURCE_DIR}/../packaging/branding/intoxicated-logo.png")
set_source_files_properties("${logo}" PROPERTIES QT_RESOURCE_ALIAS intoxicated-logo.png)
qt_add_resources(intoxicated-settings branding PREFIX / FILES "${logo}")

enable_testing()
add_test(NAME self-test COMMAND intoxicated-settings --self-test)
