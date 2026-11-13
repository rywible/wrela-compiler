// Apple's Metal compiler divides a constant u32 dividend near 2^32 wrongly: seen on an Apple M4,
// macOS 26.6.2 (Metal toolchain 17.3, metal 32023.864), with fast math on and off. `4294967295u / d` gives
// 3579139454 for d = 3 (1431655765 is right); a dividend loaded from a buffer divides right.
// Across these 103 constant dividends and 73,546 divisors the plain `/` and `%` are wrong at
// 2,468 pairs, every dividend within about a hundred of 2^32; the halved form the WGSL writer
// uses (compiler/wgsl/src/lib.rs, `halved`), `((a >> 1) / d) << 1` and one step, is wrong at
// none of the 7,575,238. No Wrela or naga in it: when this prints 0 plain pairs wrong on a
// newer macOS, the workaround can go.
//
//     swiftc -O compiler/wgsl/metal/u32_division.swift -o /tmp/u32_division && /tmp/u32_division
import Metal
var consts: [UInt32] = [4294967295, 4294967294, 4294967293, 4294967291, 4294967200, 4294900000, 4290000000, 4200000000, 4000000000, 3500000000, 3000000000, 2863311531, 2863311530, 2147483649, 2147483648, 2147483647, 2147483646, 2000000000, 1431655765, 1000000007, 65536, 1000, 7]
var s: UInt32 = 12345
for _ in 0..<40 { s = s &* 1664525 &+ 1013904223; consts.append(s | 0x80000000); consts.append(s >> 1) }
var src = "#include <metal_stdlib>\n"
for (k, c) in consts.enumerated() {
    src += """
    kernel void k\(k)(device uint* out [[buffer(0)]], device const uint* ds [[buffer(1)]], uint i [[thread_position_in_grid]]) {
        uint d = ds[i];
        uint d1 = metal::select(d, 1u, d == 0u);
        uint q = ((\(c)u >> 1u) / d1) << 1u;
        uint r = \(c)u - q * d1;
        out[4u * i] = \(c)u / d1;
        out[4u * i + 1u] = \(c)u % d1;
        out[4u * i + 2u] = q + metal::select(0u, 1u, r >= d1);
        out[4u * i + 3u] = r - metal::select(0u, d1, r >= d1);
    }

    """
}
var divs: [UInt32] = Array(1...65536).map { UInt32($0) }
divs.append(contentsOf: [0, 2147483647, 2147483648, 2147483649, 4294967295, 4294967294, 3000000000, 1431655765, 1431655766, 715827883])
for _ in 0..<4000 { s = s &* 1664525 &+ 1013904223; divs.append(s); divs.append(s >> (s & 31)) }
let n = divs.count
let dev = MTLCreateSystemDefaultDevice()!
print("device: \(dev.name), \(consts.count) constants, \(n) divisors")
let lib = try! dev.makeLibrary(source: src, options: MTLCompileOptions())
let q = dev.makeCommandQueue()!
let dbuf = dev.makeBuffer(bytes: &divs, length: n * 4, options: .storageModeShared)!
var plainBad = 0, halvedBad = 0
for (k, c) in consts.enumerated() {
    let p = try! dev.makeComputePipelineState(function: lib.makeFunction(name: "k\(k)")!)
    let out = dev.makeBuffer(length: n * 16, options: .storageModeShared)!
    let cb = q.makeCommandBuffer()!
    let e = cb.makeComputeCommandEncoder()!
    e.setComputePipelineState(p); e.setBuffer(out, offset: 0, index: 0); e.setBuffer(dbuf, offset: 0, index: 1)
    e.dispatchThreads(MTLSize(width: n, height: 1, depth: 1), threadsPerThreadgroup: MTLSize(width: 64, height: 1, depth: 1))
    e.endEncoding(); cb.commit(); cb.waitUntilCompleted()
    let g = out.contents().bindMemory(to: UInt32.self, capacity: n * 4)
    var pb: [String] = [], hb = 0
    for i in 0..<n {
        let d = divs[i] == 0 ? 1 : divs[i]
        let wq = c / d, wr = c % d
        if g[4*i] != wq || g[4*i+1] != wr { if pb.count < 4 { pb.append("d=\(divs[i]) q \(g[4*i])/\(wq) r \(g[4*i+1])/\(wr)") }; plainBad += 1 }
        if g[4*i+2] != wq || g[4*i+3] != wr { hb += 1; halvedBad += 1 }
    }
    if !pb.isEmpty || hb > 0 { print("c=\(c): plain wrong at \(pb), halved wrong \(hb)") }
}
print("plain wrong \(plainBad), halved wrong \(halvedBad) of \(consts.count * n) pairs")
