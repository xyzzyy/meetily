cmake_minimum_required(VERSION 3.16)
project(meetily-funasr-helper CXX C)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(BUILD_SHARED_LIBS OFF CACHE BOOL "" FORCE)
set(CMAKE_RUNTIME_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/bin)

if(MSVC)
    add_compile_definitions(NOMINMAX _USE_MATH_DEFINES)
    # Link the C runtime statically so the helper doesn't need the MSVC
    # redistributable installed.
    set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>")
endif()

include(FetchContent)
if(POLICY CMP0169)
    cmake_policy(SET CMP0169 OLD)
endif()
if(POLICY CMP0135)
    cmake_policy(SET CMP0135 NEW)
endif()

set(LLAMA_BUILD_TESTS OFF CACHE BOOL "" FORCE)
set(LLAMA_BUILD_EXAMPLES OFF CACHE BOOL "" FORCE)
set(LLAMA_BUILD_TOOLS OFF CACHE BOOL "" FORCE)
set(LLAMA_BUILD_SERVER OFF CACHE BOOL "" FORCE)
set(LLAMA_CURL OFF CACHE BOOL "" FORCE)
set(GGML_NATIVE OFF CACHE BOOL "" FORCE)
set(GGML_METAL OFF CACHE BOOL "" FORCE)
set(GGML_CUDA OFF CACHE BOOL "" FORCE)
set(GGML_VULKAN OFF CACHE BOOL "" FORCE)
set(GGML_HIP OFF CACHE BOOL "" FORCE)
# ggml's own thread pool instead of OpenMP, so the helper has no libgomp
# (GCC) or VCOMP140.dll (MSVC) runtime dependency. Apple's compiler has no
# OpenMP, so this matches what macOS builds already do.
set(GGML_OPENMP OFF CACHE BOOL "" FORCE)

FetchContent_Declare(
    llama
    URL https://github.com/ggml-org/llama.cpp/archive/8086439a4cea94c71a5dfb8fe4ad1546aebd640f.tar.gz
    URL_HASH SHA256=1984103666eb25bd45110a40cba22b9d4286116f26e51bbc76f6f41dc86bc7b5
)
FetchContent_MakeAvailable(llama)

find_package(Threads REQUIRED)
add_executable(funasr-helper src/main.cpp)
target_link_libraries(funasr-helper PRIVATE llama ggml Threads::Threads)
install(TARGETS funasr-helper RUNTIME DESTINATION bin)
