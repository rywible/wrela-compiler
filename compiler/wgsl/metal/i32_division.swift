// The u32 scan's twin for i32 (compiler/wgsl/metal/u32_division.swift): constant dividends near
// both ends of i32's range, and others, over 138,962 divisors of both signs (0 and MIN / -1
// left out), plain `/` and `%` against Swift's. On the Apple M4, macOS 26.6.2 (Metal 32023.864):
// wrong at 0 of 15,285,819 pairs, so the WGSL writer leaves i32 division as it is.
//
//     swiftc -O compiler/wgsl/metal/i32_division.swift -o /tmp/i32_division && /tmp/i32_division
import Metal
var consts: [Int32] = [Int32.max, Int32.max - 1, Int32.max - 2, Int32.max - 4, Int32.max - 95, Int32.max - 1000, Int32.min, Int32.min + 1, Int32.min + 2, Int32.min + 4, Int32.min + 95, Int32.min + 1000, 1_500_000_000, -1_500_000_000, 1_073_741_824, -1_073_741_824, 1000, -1000, 7, -7]
var s: UInt32 = 54321
for _ in 0..<30 { s = s &* 1664525 &+ 1013904223; consts.append(Int32(bitPattern: s)); consts.append(Int32(bitPattern: s | 0x7ff00000)); consts.append(Int32(bitPattern: s | 0x80000000) ) }
var src = "#include <metal_stdlib>\n"
for (k, c) in consts.enumerated() {
    let lit = c == Int32.min ? "(-2147483647 - 1)" : "\(c)"
    src += """
    kernel void k\(k)(device int* out [[buffer(0)]], device const int* ds [[buffer(1)]], uint i [[thread_position_in_grid]]) {
        int d = ds[i];
        out[2u * i] = \(lit) / d;
        out[2u * i + 1u] = \(lit) % d;
    }

    """
}
var divs: [Int32] = []
for d in 1...65536 { divs.append(Int32(d)); divs.append(Int32(-d)) }
divs.append(contentsOf: [Int32.max, Int32.min, Int32.max - 1, Int32.min + 1, 1_431_655_765, -1_431_655_765])
for _ in 0..<4000 { s = s &* 1664525 &+ 1013904223; let v = Int32(bitPattern: s); if v != 0 { divs.append(v) }; let w = Int32(bitPattern: s >> (s & 31)); if w != 0 { divs.append(w) } }
let n = divs.count
let dev = MTLCreateSystemDefaultDevice()!
print("device: \(dev.name), \(consts.count) constants, \(n) divisors")
let lib = try! dev.makeLibrary(source: src, options: MTLCompileOptions())
let q = dev.makeCommandQueue()!
let dbuf = dev.makeBuffer(bytes: &divs, length: n * 4, options: .storageModeShared)!
var bad = 0, pairs = 0
for (k, c) in consts.enumerated() {
    let p = try! dev.makeComputePipelineState(function: lib.makeFunction(name: "k\(k)")!)
    let out = dev.makeBuffer(length: n * 8, options: .storageModeShared)!
    let cb = q.makeCommandBuffer()!
    let e = cb.makeComputeCommandEncoder()!
    e.setComputePipelineState(p); e.setBuffer(out, offset: 0, index: 0); e.setBuffer(dbuf, offset: 0, index: 1)
    e.dispatchThreads(MTLSize(width: n, height: 1, depth: 1), threadsPerThreadgroup: MTLSize(width: 64, height: 1, depth: 1))
    e.endEncoding(); cb.commit(); cb.waitUntilCompleted()
    let g = out.contents().bindMemory(to: Int32.self, capacity: n * 2)
    var first: [String] = []
    for i in 0..<n {
        let d = divs[i]
        if c == Int32.min && d == -1 { continue }
        pairs += 1
        let wq = c / d, wr = c % d
        if g[2*i] != wq || g[2*i+1] != wr { bad += 1; if first.count < 3 { first.append("d=\(d) q \(g[2*i])/\(wq) r \(g[2*i+1])/\(wr)") } }
    }
    if !first.isEmpty { print("c=\(c): \(first)") }
}
print("i32 plain wrong \(bad) of \(pairs) pairs")
