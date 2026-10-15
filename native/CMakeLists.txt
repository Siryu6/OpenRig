# Static library wrapping NeuralAudio (NAM / RTNeural models), built by
# build.rs when the `nam` feature is enabled.
cmake_minimum_required(VERSION 3.10)
project(pedalboard_nam CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)
# Every static library in one place, for cargo
set(CMAKE_ARCHIVE_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/lib)

set(NEURALAUDIO_DIR ${CMAKE_CURRENT_SOURCE_DIR}/../third_party/NeuralAudio)
add_subdirectory(${NEURALAUDIO_DIR}/NeuralAudio NeuralAudio)

add_library(pedalboard_nam STATIC nam_wrapper.cpp)
target_link_libraries(pedalboard_nam PRIVATE NeuralAudio)
