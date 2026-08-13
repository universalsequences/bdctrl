/// Metal source compiled at launch (keeps shader iteration fast — no build step).
/// Swift-side instance structs in BeadsRenderer must stay byte-identical to the
/// structs here.
enum BeadShaders {
    static let source = """
    #include <metal_stdlib>
    using namespace metal;

    struct View { float2 viewport; float2 center; float zoom; float time; };

    constant float2 quadCorners[6] = { {-1,-1},{1,-1},{-1,1},{-1,1},{1,-1},{1,1} };
    constant float BEAD_R = 0.625;   // bead edge in quad uv (quad is radius*1.6)

    static float2 toClip(float2 world, constant View &view) {
        float2 px = (world - view.center) * view.zoom + view.viewport * 0.5;
        return float2(px.x / view.viewport.x * 2.0 - 1.0, 1.0 - px.y / view.viewport.y * 2.0);
    }

    // ---------------------------------------------------------------- beads
    struct Bead { float2 center; float radius; uint flags; float4 color; };
    struct BeadOut {
        float4 position [[position]];
        float2 uv;
        float4 color;
        uint flags [[flat]];
        float time;
    };

    vertex BeadOut beadVertex(uint v [[vertex_id]], uint i [[instance_id]],
                              constant Bead *beads [[buffer(0)]], constant View &view [[buffer(1)]]) {
        Bead b = beads[i];
        float2 uv = quadCorners[v];
        BeadOut o;
        o.position = float4(toClip(b.center + uv * b.radius * 1.6, view), 0, 1);
        o.uv = uv; o.color = b.color; o.flags = b.flags; o.time = view.time;
        return o;
    }

    fragment float4 beadFragment(BeadOut in [[stage_in]]) {
        uint f = in.flags;
        bool ready = f & 1u, blocked = f & 2u, closed = f & 4u, progressing = f & 8u;
        bool selected = f & 16u, hovered = f & 32u, faded = f & 64u, pearl = f & 256u;

        float d = length(in.uv) - BEAD_R;
        float aa = fwidth(d);
        float alpha = 1.0 - smoothstep(-aa, aa, d);
        if (alpha < 0.004) discard_fragment();

        // Treat the disc as the front hemisphere of a sphere.
        float2 pl = in.uv / BEAD_R;
        float r2 = min(dot(pl, pl), 1.0);
        float3 n = float3(pl.x, pl.y, sqrt(1.0 - r2));

        float3 base = in.color.rgb;
        if (pearl)  base = float3(0.90, 0.87, 0.80);
        if (ready)  base = mix(base, float3(0.64, 0.60, 0.51), 0.88);   // warm pearl midtone
        if (progressing) base = mix(base, float3(1.00, 0.62, 0.22), 0.75); // amber glass
        if (closed) base = mix(base, float3(0.58, 0.72, 0.68), 0.70) * 0.85; // frosted sea glass
        if (blocked && !closed && !ready) base *= 0.40;                  // smoked glass

        bool frosted = closed;
        float3 L = normalize(float3(-0.42, -0.60, 0.68));
        float3 H = normalize(L + float3(0.0, 0.0, 1.0));
        float wrap = saturate(dot(n, L) * 0.5 + 0.5);
        float spec = pow(max(dot(n, H), 0.0), frosted ? 12.0 : 110.0) * (frosted ? 0.20 : 1.15);
        float spec2 = pow(max(dot(n, normalize(float3(0.55, 0.38, 0.74))), 0.0), 34.0) * (frosted ? 0.05 : 0.20);
        float fresnel = pow(1.0 - n.z, 2.3);
        float transmit = pow(saturate(dot(n, normalize(float3(0.42, 0.60, 0.30)))), 2.0);

        float3 c = base * (0.28 + 0.55 * wrap);
        c += base * transmit * (frosted ? 0.25 : (ready ? 0.45 : 0.85)); // light exiting the far side
        c += mix(base, float3(1.0), 0.65) * fresnel * (frosted ? 0.16 : 0.30);
        c += float3(spec + spec2);
        if (pearl || ready) c += float3(0.85, 0.80, 1.0) * fresnel * 0.28;  // iridescent rim

        float glow = 0.0;
        if (ready) glow = 0.12;
        if (progressing) glow = max(glow, 0.50 + 0.28 * sin(in.time * 2.7));
        c += base * glow * (0.35 + 0.65 * (1.0 - r2));             // HDR: feeds bloom

        c *= 1.0 - 0.26 * smoothstep(0.5, 1.0, sqrt(r2));          // edge occlusion
        if (selected || hovered) c += base * fresnel * 0.9 + float3(0.08);
        if (faded) { c *= 0.38; alpha *= 0.42; }
        return float4(c, alpha * in.color.a);
    }

    fragment float4 beadShadowFragment(BeadOut in [[stage_in]]) {
        float d = length(in.uv - float2(0.10, 0.16)) / BEAD_R;
        float a = 0.40 * (1.0 - smoothstep(0.55, 1.40, d));
        if ((in.flags & 64u) != 0u) a *= 0.35;
        if ((in.flags & 4u) != 0u) a *= 0.55;
        if (a < 0.004) discard_fragment();
        return float4(0.0, 0.0, 0.0, a);
    }

    // ---------------------------------------------------------------- wires
    struct WireV { float2 position; float2 normal; float v; float along; float4 color; uint dashed; uint pad0; uint pad1; uint pad2; };
    struct WireOut {
        float4 position [[position]];
        float v;
        float along;
        float4 color;
        uint dashed [[flat]];
    };

    vertex WireOut wireVertex(uint i [[vertex_id]], constant WireV *vs [[buffer(0)]], constant View &view [[buffer(1)]]) {
        WireV w = vs[i];
        // Width resolves against zoom here, so camera changes never touch geometry.
        float halfWidth = max(0.55, 1.6 / view.zoom);
        WireOut o;
        o.position = float4(toClip(w.position + w.normal * (w.v * halfWidth), view), 0, 1);
        o.v = w.v; o.along = w.along; o.color = w.color; o.dashed = w.dashed;
        return o;
    }

    fragment float4 wireFragment(WireOut in [[stage_in]]) {
        if (in.dashed != 0u && fract(in.along / 16.0) > 0.55) discard_fragment();
        float t = abs(in.v);
        float aa = fwidth(t);
        float alpha = (1.0 - smoothstep(1.0 - aa * 1.5, 1.0, t)) * in.color.a;
        if (alpha < 0.004) discard_fragment();
        float core = 1.0 - smoothstep(0.0, 0.5, t);                // bright filament center
        return float4(in.color.rgb * (0.75 + 0.6 * core), alpha);
    }

    // ---------------------------------------------------------------- blobs
    struct Blob { float2 center; float2 size; float4 color; uint flags; uint start; uint count; uint pad; };
    struct BlobOut {
        float4 position [[position]];
        float2 world;
        float4 color;
        uint flags [[flat]];
        uint start [[flat]];
        uint count [[flat]];
        float time;
    };

    vertex BlobOut blobVertex(uint v [[vertex_id]], uint i [[instance_id]],
                              constant Blob *blobs [[buffer(0)]], constant View &view [[buffer(1)]]) {
        Blob b = blobs[i];
        float2 uv = quadCorners[v];
        float2 world = b.center + uv * b.size * 0.5;
        BlobOut o;
        o.position = float4(toClip(world, view), 0, 1);
        o.world = world; o.color = b.color; o.flags = b.flags;
        o.start = b.start; o.count = b.count; o.time = view.time;
        return o;
    }

    static float sdPolygon(float2 p, constant float2 *pts, uint start, uint count) {
        float d = dot(p - pts[start], p - pts[start]);
        float s = 1.0;
        for (uint i = start, j = start + count - 1; i < start + count; j = i, i++) {
            float2 e = pts[j] - pts[i];
            float2 w = p - pts[i];
            float2 b = w - e * clamp(dot(w, e) / dot(e, e), 0.0, 1.0);
            d = min(d, dot(b, b));
            bool3 cond = bool3(p.y >= pts[i].y, p.y < pts[j].y, e.x * w.y > e.y * w.x);
            if (all(cond) || !any(cond)) s = -s;
        }
        return s * sqrt(d);
    }

    fragment float4 blobFragment(BlobOut in [[stage_in]], constant float2 *pts [[buffer(0)]]) {
        float d = sdPolygon(in.world, pts, in.start, in.count);
        float aa = max(fwidth(d), 0.001);
        float shell = 1.0 - smoothstep(1.1, 1.1 + aa * 2.0, abs(d));
        float fill = 1.0 - smoothstep(-aa, aa, d);
        float halo = exp(-max(d, 0.0) / 30.0) * 0.05;

        // Slight iridescent tint that follows the contour normal.
        float2 g = normalize(float2(dfdx(d), dfdy(d)) + float2(1e-5, 0.0));
        float3 shellColor = mix(in.color.rgb, float3(0.82, 0.78, 0.94), saturate(0.5 - 0.5 * g.y) * 0.55);
        float pulse = (in.flags & 1u) ? (0.9 + 0.28 * sin(in.time * 1.1)) : 0.9;

        float3 c = mix(in.color.rgb * 0.85, shellColor * pulse * 1.25, saturate(shell));
        float a = max(shell * 0.85, fill * 0.055) + halo;
        if (a < 0.004) discard_fragment();
        return float4(c, a);
    }

    // ---------------------------------------------------------------- labels
    struct Label { float2 anchor; float2 sizePx; float2 uvMin; float2 uvMax; float4 color; };
    struct LabelOut { float4 position [[position]]; float2 uv; float4 color; };

    vertex LabelOut labelVertex(uint v [[vertex_id]], uint i [[instance_id]],
                                constant Label *labels [[buffer(0)]], constant View &view [[buffer(1)]]) {
        Label l = labels[i];
        float2 corner = quadCorners[v];
        float2 px = (l.anchor - view.center) * view.zoom + view.viewport * 0.5 + corner * l.sizePx * 0.5;
        LabelOut o;
        o.position = float4(px.x / view.viewport.x * 2.0 - 1.0, 1.0 - px.y / view.viewport.y * 2.0, 0, 1);
        o.uv = mix(l.uvMin, l.uvMax, corner * 0.5 + 0.5);
        o.color = l.color;
        o.color.a *= smoothstep(0.55, 0.85, view.zoom);   // fade out when zoomed far away
        return o;
    }

    fragment float4 labelFragment(LabelOut in [[stage_in]],
                                  texture2d<float> atlas [[texture(0)]], sampler s [[sampler(0)]]) {
        float4 t = atlas.sample(s, in.uv);                 // premultiplied white text
        return float4(t.rgb * in.color.rgb, t.a) * in.color.a;
    }

    // ---------------------------------------------------------------- post
    struct FSOut { float4 position [[position]]; float2 uv; };

    vertex FSOut fullscreenVertex(uint v [[vertex_id]]) {
        float2 pos[3] = { {-1,-1},{3,-1},{-1,3} };
        FSOut o;
        o.position = float4(pos[v], 0, 1);
        o.uv = float2(pos[v].x * 0.5 + 0.5, 1.0 - (pos[v].y * 0.5 + 0.5));
        return o;
    }

    fragment float4 brightFragment(FSOut in [[stage_in]],
                                   texture2d<float> scene [[texture(0)]], sampler s [[sampler(0)]]) {
        float3 c = scene.sample(s, in.uv).rgb;
        float l = max(max(c.r, c.g), c.b);
        return float4(c * smoothstep(0.72, 1.15, l), 1.0);
    }

    fragment float4 compositeFragment(FSOut in [[stage_in]],
                                      texture2d<float> scene [[texture(0)]],
                                      texture2d<float> bloom [[texture(1)]], sampler s [[sampler(0)]]) {
        float3 c = scene.sample(s, in.uv).rgb + bloom.sample(s, in.uv).rgb * 0.9;
        float2 q = in.uv * 2.0 - 1.0;
        c *= 1.0 - 0.32 * smoothstep(0.55, 1.45, length(q));       // vignette
        float peak = max(max(c.r, c.g), c.b);
        c /= 1.0 + 0.15 * max(0.0, peak - 1.0);                    // soft highlight rolloff
        return float4(c, 1.0);
    }
    """
}
