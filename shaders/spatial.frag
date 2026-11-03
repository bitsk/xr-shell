// SPDX-License-Identifier: GPL-3.0-or-later
#version 450
struct Item{vec4 centre;vec4 right_width;vec4 down_height;vec4 uvrect;vec4 logical;vec4 extra;};
layout(set=0,binding=0) uniform sampler2D panorama;
layout(set=0,binding=1) uniform sampler2D atlas;
layout(set=0,binding=2,std430) readonly buffer Draws{Item items[];};
layout(push_constant) uniform View{vec4 fov;vec4 orientation;vec4 environment;vec4 origin;}view;
layout(location=0) in vec2 local_uv;
layout(location=1) flat in int item_index;
layout(location=0) out vec4 colour;
const float PI=3.141592653589793;
float rounded(vec2 p,vec2 halfsize,float radius){vec2 q=abs(p)-halfsize+radius;return length(max(q,vec2(0)))+min(max(q.x,q.y),0.)-radius;}
void main(){
 Item item=items[item_index];vec2 size=item.logical.xy;float radius=min(item.logical.z,min(size.x,size.y)*.5);
 float distance=rounded((local_uv-.5)*size,size*.5,radius);float aa=max(fwidth(distance),.35);float mask=1.-smoothstep(-aa,aa,distance);if(mask<.001)discard;
 vec4 pixel=texture(atlas,item.uvrect.xy+local_uv*item.uvrect.zw);
 // A bounded environment blur supplies the frosted background at the default
 // non-overlapping panel layout. General scene backdrop filtering is separate.
 if(item.centre.w==1.&&pixel.a>.01){
  vec3 n=cross(item.down_height.xyz,item.right_width.xyz);vec3 world=item.centre.xyz+item.right_width.xyz*((local_uv.x-.5)*item.right_width.w)+item.down_height.xyz*((local_uv.y-.5)*item.down_height.w);
  vec3 d=normalize(world-view.origin.xyz);d=normalize(vec3(view.environment.x*d.x-view.environment.y*d.z,d.y,view.environment.y*d.x+view.environment.x*d.z));
  vec2 eq=vec2(atan(d.x,-d.z)/(2.*PI)+.5,.5-asin(clamp(d.y,-1.,1.))/PI);vec3 original=texture(panorama,eq).rgb;vec3 blurred=vec3(0.);
  for(int x=-1;x<=1;x++)for(int y=-1;y<=1;y++)blurred+=texture(panorama,eq+vec2(x,y)*.003).rgb/9.;
  pixel.rgb=max(vec3(0),pixel.rgb+(blurred-original)*((1.-pixel.a)/max(pixel.a,.01)));
 }
 float rim=(1.-smoothstep(item.logical.w-aa,item.logical.w+aa,abs(distance)))*item.extra.x;
 pixel.rgb=mix(pixel.rgb,vec3(1),rim);pixel.a=1.-(1.-pixel.a)*(1.-rim);
 colour=vec4(pixel.rgb,pixel.a*mask);
}
