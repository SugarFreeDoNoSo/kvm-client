#version 150
in vec2 v_texcoord;
uniform sampler2D tex;
uniform float width;
uniform float height;
uniform float amount;
out vec4 fragColor;

void main() {
    vec2 step_size = vec2(1.0 / max(width, 1.0), 1.0 / max(height, 1.0));
    vec4 center = texture(tex, v_texcoord);
    vec3 north = texture(tex, v_texcoord + vec2(0.0, step_size.y)).rgb;
    vec3 south = texture(tex, v_texcoord - vec2(0.0, step_size.y)).rgb;
    vec3 east = texture(tex, v_texcoord + vec2(step_size.x, 0.0)).rgb;
    vec3 west = texture(tex, v_texcoord - vec2(step_size.x, 0.0)).rgb;
    vec3 result = center.rgb + amount * (center.rgb - (north + south + east + west) * 0.25);
    vec3 low = min(center.rgb, min(min(north, south), min(east, west)));
    vec3 high = max(center.rgb, max(max(north, south), max(east, west)));
    fragColor = vec4(clamp(result, low, high), center.a);
}
