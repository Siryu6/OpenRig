#!/bin/sh
#
# x86_64 post-build step: drop what Buildroot installs for building the
# Intel (iris) shaders, which happens on the build machine. Mesa only loads
# libLLVM and libSPIRV-Tools at run time; clang, libclc and the SPIR-V tools
# are ~180 MB the image does not use.

set -eu

TARGET=$1

rm -rf "$TARGET"/usr/lib/libclang.so* "$TARGET"/usr/lib/libclang-cpp.so* \
	"$TARGET"/usr/lib/libLLVMSPIRVLib.so* "$TARGET"/usr/lib/clang \
	"$TARGET"/usr/share/clc
rm -f "$TARGET"/usr/bin/diagtool "$TARGET"/usr/bin/llvm-spirv \
	"$TARGET"/usr/bin/spirv-* "$TARGET"/usr/bin/clang* \
	"$TARGET"/usr/bin/llvm-* "$TARGET"/usr/bin/llc "$TARGET"/usr/bin/opt

# Nothing left may need what was removed
missing=$(find "$TARGET/usr/lib" "$TARGET/usr/bin" "$TARGET/lib" -type f 2>/dev/null |
	xargs -r readelf -d 2>/dev/null |
	sed -n 's/.*(NEEDED).*\[\(libclang[^]]*\|libLLVMSPIRV[^]]*\)\].*/\1/p' | sort -u)
if [ -n "$missing" ]; then
	echo "post-build: still needed: $missing" >&2
	exit 1
fi
