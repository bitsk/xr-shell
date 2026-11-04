// SPDX-License-Identifier: GPL-3.0-or-later
#version 450
struct Item{vec4 centre;vec4 right_width;vec4 down_height;vec4 uvrect;vec4 logical;vec4 extra;};
layout(set=0,binding=0) uniform sampler2D panorama;
layout(set=0,binding=1) uniform sampler2D atlas;
layout(set=0,binding=2,std430) readonly buffer Draws{Item items[];};
layout(push_constant) uniform View{vec4 fov;vec4 orientation;vec4 environment;vec4 origin;}view;
layout(location=0) in vec2 local_uv;
layout(location=1) flat in int item_index;
layout(location=2) flat in int side_face;
layout(location=3) in vec3 world_position;
layout(location=4) in vec3 world_normal;
layout(location=0) out vec4 colour;
const float PI=3.141592653589793;
float rounded(vec2 p,vec2 halfsize,float radius){vec2 q=abs(p)-halfsize+radius;return length(max(q,vec2(0)))+min(max(q.x,q.y),0.)-radius;}
vec3 linear_colour(vec3 c){return mix(c/12.92,pow((c+.055)/1.055,vec3(2.4)),step(vec3(.04045),c));}
vec3 display_colour(vec3 c){c=max(c,vec3(0));return mix(c*12.92,1.055*pow(c,vec3(1./2.4))-.055,step(vec3(.0031308),c));}
vec2 environment_uv(vec3 d){d=normalize(vec3(view.environment.x*d.x-view.environment.y*d.z,d.y,view.environment.y*d.x+view.environment.x*d.z));return vec2(atan(d.x,-d.z)/(2.*PI)+.5,.5-asin(clamp(d.y,-1.,1.))/PI);}
void main(){
 Item item=items[item_index];vec2 size=item.logical.xy;float material=item.centre.w;float radius=min(item.logical.z,min(size.x,size.y)*.5);
 if(material==4.){
  float distance=rounded((local_uv-.5)*size,max(vec2(1),size*.5-vec2(24)),radius);
  float alpha=.17*(1.-smoothstep(-4.,24.,distance));if(alpha<.001)discard;
  colour=vec4(linear_colour(vec3(35,60,96)/255.),alpha);return;
 }
 vec3 normal=normalize(world_normal),toward_eye=normalize(view.origin.xyz-world_position);
 if(side_face==1){
  if(dot(normal,toward_eye)<=0.)discard;
  float light=clamp(dot(normal,normalize(vec3(-.5,1,1))),0.,1.);float fresnel=pow(1.-abs(dot(normal,toward_eye)),3.);
  vec3 tint=linear_colour(mix(vec3(167,192,224),vec3(243,250,255),.2+.65*light)/255.);
  colour=vec4(tint+vec3(.035,.055,.075)*fresnel,material==3.?.52:.7);return;
 }
 float distance=rounded((local_uv-.5)*size,size*.5,radius);float aa=max(fwidth(distance),.3);float mask=1.-smoothstep(-aa,aa,distance);if(mask<.001)discard;
 vec4 pixel=texture(atlas,item.uvrect.xy+clamp(local_uv,0.,1.)*item.uvrect.zw);
 if(material==1.&&pixel.a>.01){
  vec2 eq=environment_uv(normalize(world_position-view.origin.xyz));vec3 original=texture(panorama,eq).rgb,blurred=vec3(0);
  vec2 refract_offset=(local_uv-.5)*.0015+normal.xy*.0007;
  for(int x=-1;x<=1;x++)for(int y=-1;y<=1;y++)blurred+=texture(panorama,eq+refract_offset+vec2(x,y)*.003).rgb/9.;
  vec3 transmitted=blurred*vec3(.98,1.0,1.02);
  pixel.rgb=max(vec3(0),pixel.rgb+(transmitted-original)*((1.-pixel.a)/max(pixel.a,.01)));
  float spec=mix(.10,.02,clamp((local_uv.x+local_uv.y)*.5,0.,1.));
  pixel.rgb=linear_colour(mix(display_colour(pixel.rgb),vec3(1),spec));
  // Cool optical glaze, measured against the Qingkong reference over its sky.
  // Text, application icons and client content use other material branches.
  pixel.rgb=linear_colour(clamp(display_colour(pixel.rgb)+vec3(.020,.020,.028),0.,1.));
 }
 if(material==3.&&pixel.a>.01){
  float bevel=(1.-smoothstep(0.,3.,abs(distance)))*.12;
  pixel.rgb=linear_colour(mix(display_colour(pixel.rgb),vec3(.82,.92,1),bevel));
 }
 vec2 q=(local_uv-.5)*size;vec2 edge=max(abs(q)-size*.5+radius,0.);vec2 edge_normal;
 if(length(edge)>.001)edge_normal=normalize(edge)*sign(q);
 else edge_normal=abs(q.x)-size.x*.5>abs(q.y)-size.y*.5?vec2(sign(q.x),0):vec2(0,sign(q.y));
 float border=item.extra.x;
 if(material==1.)border=mix(.16,.50,clamp(-edge_normal.x-edge_normal.y,0.,1.));
 float rim=(1.-smoothstep(item.logical.w-aa,item.logical.w+aa,abs(distance)))*border;
 pixel.rgb=mix(pixel.rgb,vec3(1),rim);pixel.a=1.-(1.-pixel.a)*(1.-rim);
 colour=vec4(pixel.rgb,pixel.a*mask);
}
