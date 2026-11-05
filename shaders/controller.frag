// SPDX-License-Identifier: GPL-3.0-or-later
#version 450
layout(set=0,binding=0) uniform sampler2D artwork;
layout(push_constant) uniform View {vec4 fov;vec4 orientation;vec4 origin;vec4 model_q;vec4 model_p;vec4 controls;vec4 hit_normal;}view;
layout(location=0) in vec2 texture_uv;
layout(location=1) in vec3 world_normal;
layout(location=0) out vec4 colour;
void main(){
 if(view.model_p.w>1.5){
  float r=length(texture_uv);if(r>1.)discard;
  vec3 tint=mix(vec3(.45,.55,.65),vec3(.12,.6,1.),view.controls.w);
  if(view.controls.x>.5)tint=vec3(.38,.9,1.);
  colour=vec4(mix(tint,vec3(1),smoothstep(.55,.7,r)),1);return;
 }
 if(view.model_p.w>.5){
  float edge=1.-smoothstep(.1,.5,abs(texture_uv.x-.5));
  float fade=smoothstep(0.,.08,texture_uv.y)*(1.-smoothstep(.94,1.,texture_uv.y));
  colour=vec4(mix(vec3(.12,.48,.9),vec3(.38,.9,1),view.controls.x),edge*fade*.8);return;
 }
 vec4 pixel=texture(artwork,texture_uv);if(pixel.a<.5)discard;
 vec3 n=normalize(world_normal);float light=.42+.58*abs(dot(n,normalize(vec3(-.4,.8,.6))));
 colour=vec4(pixel.rgb*light,1);
}
