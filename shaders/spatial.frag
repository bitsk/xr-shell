// SPDX-License-Identifier: GPL-3.0-or-later
#version 450
struct Item{vec4 centre;vec4 right_width;vec4 down_height;vec4 uvrect;vec4 logical;vec4 extra;vec4 tint;vec4 controls;};
layout(set=0,binding=1) uniform sampler2D atlas;
layout(set=0,binding=2,std430) readonly buffer Draws{Item items[];};
layout(push_constant) uniform View{vec4 fov;vec4 orientation;vec4 environment;vec4 origin;}view;
layout(location=0) in vec2 local_uv;
layout(location=1) flat in int item_index;
layout(location=2) flat in int side_face;
layout(location=3) in vec3 world_position;
layout(location=4) in vec3 world_normal;
layout(location=0) out vec4 colour;
layout(constant_id=0) const int opaque_pass=0;
float rounded(vec2 p,vec2 halfsize,float radius){vec2 q=abs(p)-halfsize+radius;return length(max(q,vec2(0)))+min(max(q.x,q.y),0.)-radius;}
vec3 linear_colour(vec3 c){return mix(c/12.92,pow((c+.055)/1.055,vec3(2.4)),step(vec3(.04045),c));}
vec3 display_colour(vec3 c){c=max(c,vec3(0));return mix(c*12.92,1.055*pow(c,vec3(1./2.4))-.055,step(vec3(.0031308),c));}
vec4 over(vec4 front,vec4 back){return front+back*(1.-front.a);}
void main(){
 Item item=items[item_index];vec2 size=item.logical.xy;float material=item.centre.w;
 float radius=min(item.logical.z,min(size.x,size.y)*.5);
 if(material==4.){
  float spread=max(item.logical.w,1.);
  float distance=rounded((local_uv-.5)*size,max(vec2(1),size*.5-vec2(spread)),radius);
  // A translucent face must never reveal a solid shadow painted underneath it.
  float outside=smoothstep(-.5,2.,distance);
  float alpha=.06*outside*exp(-pow(max(distance,0.)/(spread*.48),2.))*(1.-smoothstep(spread*.7,spread,distance));
  if(alpha<.001||opaque_pass==1)discard;
  colour=vec4(linear_colour(vec3(.16))*alpha,alpha);return;
 }
 if(material==6.){
  float distance=rounded((local_uv-.5)*size,max(vec2(1),size*.5-vec2(5.,4.)),2.);
  float core=1.-smoothstep(-.6,.6,distance);
  float alpha=core;
  if(alpha<.001||opaque_pass==1)discard;
  colour=vec4(linear_colour(vec3(.26,.72,1.))*alpha,alpha);return;
 }
 vec3 normal=normalize(world_normal),toward_eye=normalize(view.origin.xyz-world_position);
 vec3 light=normalize(vec3(-.5,1.,1.));
 float facing=clamp(dot(normal,toward_eye),0.,1.);
 float fresnel=.04+.96*pow(1.-facing,5.);
 // Each eye computes its own half-vector. No UV-baked highlight or sky transmission.
 float spec=pow(max(dot(normal,normalize(light+toward_eye)),0.),64.);
 if(side_face==1){
  if(facing<=0.||opaque_pass==1)discard;
  float alpha=material==1.?.24:min(.52,item.tint.a);
  vec3 tint=linear_colour(material==1.?vec3(.95,.965,.985):mix(vec3(.65,.75,.88),vec3(.95,.98,1.),.2+.65*max(dot(normal,light),0.)));
  colour=vec4((tint+vec3(.08)*fresnel)*alpha,alpha);return;
 }
 vec2 point=(local_uv-.5)*size;
 float distance=rounded(point,size*.5,radius),aa=max(fwidth(distance),.3);
 float mask=1.-smoothstep(-aa,aa,distance);if(mask<.001)discard;
 vec2 atlas_uv=item.uvrect.xy+clamp(local_uv,0.,1.)*item.uvrect.zw;
 vec4 glyph=texture(atlas,atlas_uv);
 // Cairo's bytes are premultiplied in sRGB. Undo that representation
 // before linear-light premultiplied compositing (the sampler decodes sRGB).
 glyph.rgb=glyph.a>.0001?linear_colour(clamp(display_colour(glyph.rgb)/glyph.a,0.,1.))*glyph.a:vec3(0);
 vec4 base=vec4(0);
 if(material==1.||material==3.||material==5.){
  vec4 tint=item.tint;
  tint.rgb=mix(tint.rgb,vec3(.78,.88,1.),item.controls.x*.35);
  tint.rgb=mix(tint.rgb,vec3(.55,.74,.98),item.controls.y*.25);
  tint.a=mix(tint.a,max(tint.a,.32),item.controls.x);
  if(material==5.){
   float track=rounded(point,vec2(size.x*.5,11.),11.);
   float track_alpha=1.-smoothstep(-aa,aa,track);
   vec3 track_colour=mix(vec3(.75,.82,.92),vec3(.15,.52,.96),1.-smoothstep(item.controls.z-.002,item.controls.z+.002,local_uv.x));
   base=vec4(linear_colour(track_colour)*track_alpha,track_alpha);
   float cx=clamp(size.x*item.controls.z,14.,size.x-14.)-size.x*.5;
   float knob=length(point-vec2(cx,0.))-14.;
   float knob_alpha=1.-smoothstep(-aa,aa,knob);
   base=over(vec4(linear_colour(vec3(.98,.99,1.))*knob_alpha,knob_alpha),base);
  }else{
   vec3 lit=linear_colour(tint.rgb);
   if(material==1.){lit+=vec3(.045)*spec+vec3(.02)*fresnel;}
   else{lit+=vec3(.16)*spec+vec3(.08,.10,.13)*fresnel;}
   base=vec4(lit*tint.a,tint.a);
  }
 }
 vec4 pixel=over(glyph,base);
 if(item.controls.w>.5){
  float grip=rounded(point,vec2(size.x*.30,2.5),2.5);
  float alpha=1.-smoothstep(-aa,aa,grip);
  pixel=over(vec4(linear_colour(vec3(.216,.565,.98))*alpha,alpha),pixel);
 }
 float rim=(1.-smoothstep(item.logical.w-aa,item.logical.w+aa,abs(distance)))*item.extra.x;
 if(material==1.){
  float lit_edge=max(1.-local_uv.y,(1.-local_uv.x)*.6);
  float outer=1.-smoothstep(0.,.55+aa,abs(distance));
  rim=outer*mix(.08,.28,lit_edge);
 }else{rim*=clamp(.45+fresnel+spec,0.,1.);}

 pixel=over(vec4(vec3(rim),rim),pixel);
 colour=pixel*mask;
 if(colour.a<.001)discard;
 if(opaque_pass==1){if(colour.a<.995)discard;}
 else if(colour.a>=.995)discard;
}
