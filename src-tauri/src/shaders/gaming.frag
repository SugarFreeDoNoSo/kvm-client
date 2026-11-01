#ifdef GL_ES
precision highp float;
#endif
varying vec2 v_texcoord;
// Copyright (c) 2025, Qualcomm Innovation Center, Inc. All rights reserved.
// SPDX-License-Identifier: BSD-3-Clause
// Adapted for kvm-client; complete license in THIRD_PARTY_NOTICES.md.
uniform sampler2D tex;
uniform float input_width;
uniform float input_height;
uniform float output_width;
uniform float output_height;
uniform float native_size;
uniform float amount;
uniform float zoom;
uniform float anti_ringing;
uniform float edge_threshold;
uniform float downscale_aa;

vec2 source_size() {
    return max(vec2(input_width, input_height), vec2(1.0));
}

vec2 viewport_size() {
    return max(vec2(output_width, output_height), vec2(1.0));
}

vec2 source_uv() {
    if (native_size > 0.5) {
        vec2 content_size = source_size() * clamp(zoom, 1.0, 4.0);
        // Integer origin preserves pixel boundaries at integer zoom, including
        // odd viewport differences; oversized content is cropped symmetrically.
        vec2 origin = floor((viewport_size() - content_size) * 0.5);
        return (v_texcoord * viewport_size() - origin) / content_size;
    }
    return v_texcoord;
}

vec4 sample_source(vec2 uv) {
    // Clamp to texel centers so black borders never bleed into image edges.
    vec2 half_pixel = vec2(0.5) / source_size();
    return texture2D(tex, clamp(uv, half_pixel, vec2(1.0) - half_pixel));
}

bool use_downscale_aa() {
    return downscale_aa > 0.5 && native_size < 0.5 &&
        any(greaterThan(source_size(), viewport_size()));
}

vec4 sample_area(vec2 uv) {
    // Integrate the destination pixel's source footprint with equal-area
    // strata. Never widen an upscaled axis. At most 8x8 bilinear taps keep
    // cost bounded; larger reductions use the same full footprint/coarser grid.
    vec2 footprint = max(source_size() / viewport_size(), vec2(1.0));
    vec2 count = clamp(ceil(footprint), vec2(1.0), vec2(8.0));
    vec4 total = vec4(0.0);
    for (int y = 0; y < 8; ++y) {
        for (int x = 0; x < 8; ++x) {
            if (float(x) < count.x && float(y) < count.y) {
                vec2 cell = (vec2(float(x), float(y)) + vec2(0.5)) / count;
                vec2 offset = (cell - vec2(0.5)) * footprint / source_size();
                total += sample_source(uv + offset);
            }
        }
    }
    return total / (count.x * count.y);
}

// SGSR v1 port of Qualcomm's 12-tap reconstruction and adaptive sharpening.
// The portable form spells out gather components for GLSL 1.20/1.50 support.
// Green-channel edge mode matches OperationMode=1 in the reference shader.
// See THIRD_PARTY_NOTICES.md for source, modifications and BSD-3-Clause terms.
float fast_lanczos2(float x) {
    float a = x - 4.0;
    float b = x * a - a;
    a *= a;
    return b * a;
}

vec2 weight_y(float dx, float dy, float c, float deviation) {
    float x = (dx * dx + dy * dy) * 0.55 + clamp(abs(c) * deviation, 0.0, 1.0);
    float w = fast_lanczos2(x);
    return vec2(w, w * c);
}

float green_at(vec2 pixel) {
    return sample_source((pixel + vec2(0.5)) / source_size()).g;
}

void main() {
    vec2 uv = source_uv();
    if (any(lessThan(uv, vec2(0.0))) || any(greaterThan(uv, vec2(1.0)))) {
        gl_FragColor = vec4(0.0, 0.0, 0.0, 1.0);
        return;
    }
    if (use_downscale_aa()) {
        // Downscaling uses area filtering instead of SGSR edge sharpening.
        gl_FragColor = vec4(clamp(sample_area(uv).rgb, 0.0, 1.0), 1.0);
        return;
    }
    vec4 color = sample_source(uv);
    vec2 position = uv * source_size() + vec2(-0.5, 0.5);
    vec2 base = floor(position);
    vec2 p = position - base;
    // OpenGL textureGather component order: (i0,j1), (i1,j1),
    // (i1,j0), (i0,j0). Explicit samples avoid requiring textureGather.
    vec4 left = vec4(green_at(base + vec2(-1.0, 0.0)),
                     green_at(base),
                     green_at(base + vec2(0.0, -1.0)),
                     green_at(base + vec2(-1.0, -1.0)));
    float edge_vote = abs(left.z - left.y) + abs(color.g - left.y) + abs(color.g - left.z);
    if (edge_vote > clamp(edge_threshold, 1.0 / 255.0, 32.0 / 255.0)) {
        vec4 right = vec4(green_at(base + vec2(1.0, 0.0)),
                          green_at(base + vec2(2.0, 0.0)),
                          green_at(base + vec2(2.0, -1.0)),
                          green_at(base + vec2(1.0, -1.0)));
        vec4 up_down = vec4(green_at(base + vec2(0.0, -2.0)),
                            green_at(base + vec2(1.0, -2.0)),
                            green_at(base + vec2(1.0, 1.0)),
                            green_at(base + vec2(0.0, 1.0)));
        float mean = (left.y + left.z + right.x + right.w) * 0.25;
        left -= vec4(mean);
        right -= vec4(mean);
        up_down -= vec4(mean);
        float center_y = color.g - mean;
        float sum = dot(abs(left), vec4(1.0)) + dot(abs(right), vec4(1.0)) + dot(abs(up_down), vec4(1.0));
        float deviation = 2.181818 / max(sum, 0.000001);
        vec2 weighted = weight_y(p.x, p.y + 1.0, up_down.x, deviation);
        weighted += weight_y(p.x - 1.0, p.y + 1.0, up_down.y, deviation);
        weighted += weight_y(p.x - 1.0, p.y - 2.0, up_down.z, deviation);
        weighted += weight_y(p.x, p.y - 2.0, up_down.w, deviation);
        weighted += weight_y(p.x + 1.0, p.y - 1.0, left.x, deviation);
        weighted += weight_y(p.x, p.y - 1.0, left.y, deviation);
        weighted += weight_y(p.x, p.y, left.z, deviation);
        weighted += weight_y(p.x + 1.0, p.y, left.w, deviation);
        weighted += weight_y(p.x - 1.0, p.y - 1.0, right.x, deviation);
        weighted += weight_y(p.x - 2.0, p.y - 1.0, right.y, deviation);
        weighted += weight_y(p.x - 2.0, p.y, right.z, deviation);
        weighted += weight_y(p.x - 1.0, p.y, right.w, deviation);
        float final_y = center_y;
        if (abs(weighted.x) > 0.000001) {
            final_y = weighted.y / weighted.x;
        }
        float max_y = max(max(left.y, left.z), max(right.x, right.w));
        float min_y = min(min(left.y, left.z), min(right.x, right.w));
        // SGSR's documented EdgeSharpness range is [1,2].
        float edge_sharpness = 1.0 + clamp(amount, 0.0, 1.0);
        final_y = clamp(edge_sharpness * final_y, min_y, max_y);
        float delta_y = clamp(final_y - center_y, -23.0 / 255.0, 23.0 / 255.0);
        color.rgb = clamp(color.rgb + vec3(delta_y), vec3(0.0), vec3(1.0));
    }
    gl_FragColor = vec4(color.rgb, 1.0);
}
