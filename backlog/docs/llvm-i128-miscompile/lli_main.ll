target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-linux-gnu"

@buf = global [64 x i64] zeroinitializer, align 16
@fmt = private constant [23 x i8] c"hi=%016llx lo=%016llx\0A\00"
declare i32 @printf(ptr, ...)

define void @f(ptr %xmm, i8 %dstb, i8 %ab, i8 %srcb, i1 %isf64) {
bb1:
  %isrc = zext i8 %srcb to i64
  %psrc = getelementptr inbounds nuw i128, ptr %xmm, i64 %isrc
  %vsrc = load i128, ptr %psrc, align 16
  %ia = zext i8 %ab to i64
  %ok1 = icmp ult i64 %ia, 32
  br i1 %ok1, label %bb2, label %panic
bb2:
  %id = zext i8 %dstb to i64
  %ok2 = icmp ult i64 %id, 32
  br i1 %ok2, label %bb844, label %panic
bb844:
  %pa = getelementptr inbounds nuw i128, ptr %xmm, i64 %ia
  %va = load i128, ptr %pa, align 16
  %notmask = select i1 %isf64, i128 -18446744073709551616, i128 -4294967296
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

define i32 @main() {
  %p12 = getelementptr [64 x i64], ptr @buf, i64 0, i64 12
  store i64 1068944782, ptr %p12, align 16
  %p13 = getelementptr [64 x i64], ptr @buf, i64 0, i64 13
  store i64 1229782938247303441, ptr %p13, align 8
  %p8 = getelementptr [64 x i64], ptr @buf, i64 0, i64 8
  store i64 -4919131752989213765, ptr %p8, align 16
  %p9 = getelementptr [64 x i64], ptr @buf, i64 0, i64 9
  store i64 -6148914691236517206, ptr %p9, align 8
  %p4 = getelementptr [64 x i64], ptr @buf, i64 0, i64 4
  store i64 2459565876208275729, ptr %p4, align 16
  %p5 = getelementptr [64 x i64], ptr @buf, i64 0, i64 5
  store i64 4919131752912833934, ptr %p5, align 8
  call void @f(ptr @buf, i8 2, i8 4, i8 6, i1 false)
  %lo = load i64, ptr %p4, align 16
  %hi = load i64, ptr %p5, align 8
  call i32 (ptr, ...) @printf(ptr @fmt, i64 %hi, i64 %lo)
  ret i32 0
}
