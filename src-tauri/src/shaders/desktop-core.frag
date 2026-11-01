#version 150
in vec2 v_texcoord;
out vec4 fragColor;
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
    return texture(tex, clamp(uv, half_pixel, vec2(1.0) - half_pixel));
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

// Separable Catmull-Rom reconstruction. Anti-ringing blends toward the
// nearest 2x2 source range; zero retains the unconstrained cubic response.
vec4 cubic_weights(float t) {
    float t2 = t * t;
    float t3 = t2 * t;
    return vec4(-0.5*t + t2 - 0.5*t3,
                1.0 - 2.5*t2 + 1.5*t3,
                0.5*t + 2.0*t2 - 1.5*t3,
                -0.5*t2 + 0.5*t3);
}

void main() {
    vec2 uv = source_uv();
    if (any(lessThan(uv, vec2(0.0))) || any(greaterThan(uv, vec2(1.0)))) {
        fragColor = vec4(0.0, 0.0, 0.0, 1.0);
        return;
    }
    if (use_downscale_aa()) {
        // No sharpening when reducing: it would restore removed aliasing.
        fragColor = clamp(sample_area(uv), 0.0, 1.0);
        return;
    }
    vec2 size = source_size();
    vec2 position = uv * size - vec2(0.5);
    vec2 base = floor(position);
    vec2 fraction = position - base;
    vec4 wx = cubic_weights(fraction.x);
    vec4 wy = cubic_weights(fraction.y);
    vec4 result = vec4(0.0);
    for (int y = 0; y < 4; ++y) {
        vec4 row = vec4(0.0);
        for (int x = 0; x < 4; ++x) {
            vec2 tap = (base + vec2(float(x - 1), float(y - 1)) + vec2(0.5)) / size;
            row += sample_source(tap) * wx[x];
        }
        result += row * wy[y];
    }
    vec2 pixel = vec2(1.0) / size;
    vec2 top_left = (base + vec2(0.5)) / size;
    vec4 a = sample_source(top_left);
    vec4 b = sample_source(top_left + vec2(pixel.x, 0.0));
    vec4 c = sample_source(top_left + vec2(0.0, pixel.y));
    vec4 d = sample_source(top_left + pixel);
    vec3 low = min(min(a.rgb, b.rgb), min(c.rgb, d.rgb));
    vec3 high = max(max(a.rgb, b.rgb), max(c.rgb, d.rgb));
    float ring_control = clamp(anti_ringing, 0.0, 1.0);
    result.rgb = mix(result.rgb, clamp(result.rgb, low, high), ring_control);
    vec3 neighbors = sample_source(uv + vec2(pixel.x, 0.0)).rgb
        + sample_source(uv - vec2(pixel.x, 0.0)).rgb
        + sample_source(uv + vec2(0.0, pixel.y)).rgb
        + sample_source(uv - vec2(0.0, pixel.y)).rgb;
    result.rgb += clamp(amount, 0.0, 1.0) * 0.35 * (result.rgb - neighbors * 0.25);
    result.rgb = mix(result.rgb, clamp(result.rgb, low, high), ring_control);
    fragColor = clamp(result, 0.0, 1.0);
}
