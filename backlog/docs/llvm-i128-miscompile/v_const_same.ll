target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-linux-gnu"

define void @f(ptr %xmm, i8 %dstb, i8 %ab, i8 %srcb, i1 %isf64) {
bb1:
  %isrc = zext i8 %srcb to i64
  %ia = zext i8 %ab to i64
  %ok1 = icmp ult i64 %ia, 32
  br i1 %ok1, label %bb2, label %panic

bb2:
  %id = zext i8 %dstb to i64
  %ok2 = icmp ult i64 %id, 32
  br i1 %ok2, label %bb844, label %panic

bb844:
  %psrc = getelementptr inbounds nuw i128, ptr %xmm, i64 %isrc
  %vsrc = load i128, ptr %psrc, align 16
  %pa = getelementptr inbounds nuw i128, ptr %xmm, i64 %ia
  %va = load i128, ptr %pa, align 16
  %notmask = or i128 -4294967296, 0
  %hi = and i128 %va, %notmask
  %m = xor i128 %notmask, -1
  %lo = and i128 %vsrc, %m
  %pd = getelementptr inbounds nuw i128, ptr %xmm, i64 %id
  %r = or disjoint i128 %hi, %lo
  store i128 %r, ptr %pd, align 16
  ret void

panic:
  unreachable
}
