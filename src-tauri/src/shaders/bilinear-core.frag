#version 150
in vec2 v_texcoord;
uniform sampler2D tex;
out vec4 fragColor;

void main() {
    fragColor = texture(tex, v_texcoord);
}
