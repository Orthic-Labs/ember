// Histogram of a rendered 8-bit frame (issue #213): the bins of lightcraft_raster's
// Histogram::of_srgb8, one thread per sampled pixel, counted with atomics. Bindings: P (params),
// src (packed RGBA8 words, red in the low byte), hist (256 red, 256 green, 256 blue and 256 luma
// counters, then the sampled total).
// P: 0 window w, 1 window h, 2 frame w, 3 window x, 4 window y, 5 sample stride, 6 sampled w,
// 7 sampled h.
@compute @workgroup_size(16, 16)
fn histogram(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= P[6] || gid.y >= P[7] {
        return;
    }
    let x = P[3] + gid.x * P[5];
    let y = P[4] + gid.y * P[5];
    let px = src[y * P[2] + x];
    let r = px & 255u;
    let g = (px >> 8u) & 255u;
    let b = (px >> 16u) & 255u;
    let l = min((r * 54u + g * 183u + b * 19u) >> 8u, 255u);
    atomicAdd(&hist[r], 1u);
    atomicAdd(&hist[256u + g], 1u);
    atomicAdd(&hist[512u + b], 1u);
    atomicAdd(&hist[768u + l], 1u);
    atomicAdd(&hist[1024u], 1u);
}
