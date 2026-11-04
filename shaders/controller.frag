// SPDX-License-Identifier: GPL-3.0-or-later
#version 450
layout(set=0,binding=0) uniform sampler2D artwork;
layout(push_constant) uniform View {vec4 fov;vec4 orientation;vec4 origin;vec4 model_q;vec4 model_p;vec4 controls;}view;
layout(location=0) in vec2 texture_uv;
layout(location=1) in vec3 world_normal;
layout(location=0) out vec4 colour;
void main(){
 if(view.model_p.w>.5){colour=vec4(mix(vec3(.12,.48,.9),vec3(.38,.9,1),view.controls.x),1);return;}
 vec4 pixel=texture(artwork,texture_uv);if(pixel.a<.5)discard;
 vec3 n=normalize(world_normal);float light=.42+.58*abs(dot(n,normalize(vec3(-.4,.8,.6))));
 colour=vec4(pixel.rgb*light,1);
}
