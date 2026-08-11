#!/bin/sh
# Reproduce the LLVM x86-64 i128 miscompile behind task-223/229. aarch64 is a
# control: the same IR is correct there. Exits 0 if the bug is
# present (as expected on an affected toolchain), 1 if the toolchain has been fixed.
set -e
cd "$(dirname "$0")"
: "${LLC:=llc}"
: "${CC:=cc}"
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT

echo "== $($LLC --version | grep -i 'LLVM version') =="
echo "   (correct on 19.1.7 and 20.1.8; miscompiled on 21.1.8, 22.1.2 and 22.1.8 —"
echo "    a regression, see the version table in the doc beside this script)"
echo
echo "-- variant matrix (llc -O2) ------------------------------------------"
for v in v_sel_pred v_sel_same v_const_pred v_const_same; do
  $LLC -O2 -filetype=obj -relocation-model=pic -o "$tmp/$v.o" "$v.ll"
  $CC -O0 -w -o "$tmp/$v" drv.c "$tmp/$v.o"
  printf '  %-14s %s\n' "$v" "$("$tmp/$v" | tail -1)"
done
echo "  (only v_sel_pred should say MISCOMPILED: it is the one with BOTH an i128"
echo "   select-of-constants mask AND the other operand loaded in a predecessor block)"
echo
echo "-- every llc opt level, v_sel_pred -----------------------------------"
for O in 0 1 2 3; do
  $LLC "-O$O" -filetype=obj -relocation-model=pic -o "$tmp/o.o" v_sel_pred.ll
  $CC -O0 -w -o "$tmp/o" drv.c "$tmp/o.o"
  printf '  llc -O%s  %s\n' "$O" "$("$tmp/o" | tail -1)"
done
echo
echo "-- LLVM against itself, same module ----------------------------------"
echo "  want         hi=aaaaaaaaaaaaaaaa lo=bbbbbbbb3fb6cd8e"
printf '  interpreter  %s\n' "$(lli --force-interpreter lli_main.ll | tail -1)"
printf '  jit          %s\n' "$(lli lli_main.ll | tail -1)"

echo
echo "-- aarch64 (optional: needs clang + lld + qemu-aarch64) --------------"
if command -v qemu-aarch64 >/dev/null && command -v clang >/dev/null; then
  # Same IR, retargeted. broken_ctl deliberately zeroes the high half in the IR: it is
  # the positive control proving this leg can detect a wrong answer at all.
  for v in v_sel_pred v_sel_same v_const_pred v_const_same; do
    sed -e 's|^target datalayout = .*|target datalayout = "e-m:e-i8:8:32-i16:16:32-i64:64-i128:128-n32:64-S128"|' \
        -e 's|^target triple = .*|target triple = "aarch64-unknown-linux-gnu"|' "$v.ll" > "$tmp/arm_$v.ll"
  done
  sed 's|%hi = and i128 %va, %notmask|%hi = and i128 %va, 4294967295|' \
      "$tmp/arm_v_sel_pred.ll" > "$tmp/arm_broken_ctl.ll"
  for v in v_sel_pred v_sel_same v_const_pred v_const_same broken_ctl; do
    $LLC -mtriple=aarch64-unknown-linux-gnu -O2 -filetype=obj -o "$tmp/a_$v.o" "$tmp/arm_$v.ll"
    clang --target=aarch64-linux-gnu -nostdlib -static -O0 -w -fuse-ld=lld \
          -o "$tmp/a_$v" drv_free.c "$tmp/a_$v.o"
    if qemu-aarch64 "$tmp/a_$v"; then r=correct; else r=MISCOMPILED; fi
    printf '  %-14s %s\n' "$v" "$r"
  done
  echo "  (all four should be correct; broken_ctl MUST say MISCOMPILED or this leg proves nothing)"
else
  echo "  skipped: clang and/or qemu-aarch64 not on PATH"
fi

$LLC -O2 -filetype=obj -relocation-model=pic -o "$tmp/f.o" v_sel_pred.ll
$CC -O0 -w -o "$tmp/f" drv.c "$tmp/f.o"
if "$tmp/f" >/dev/null; then
  echo; echo "TOOLCHAIN IS FIXED — see the doc before removing the workaround."; exit 1
fi
echo; echo "bug present (expected on an affected toolchain)"
