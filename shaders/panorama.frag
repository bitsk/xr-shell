// SPDX-License-Identifier: GPL-3.0-or-later
#version 450
layout(location=0) in vec2 uv;
layout(location=0) out vec4 colour;
layout(set=0,binding=0) uniform sampler2D panorama;
layout(set=0,binding=3) uniform sampler2D passthrough;
layout(set=0,binding=4,std430) readonly buffer CameraCalibration { vec4 camera[11]; } calibration;
layout(push_constant) uniform View{vec4 fov;vec4 orientation;vec4 environment;vec4 origin;}view;
const float PI=3.141592653589793;
void main(){
if(view.environment.z>.5){
    int eye=int(view.environment.w+.5),base=eye*5;
    vec3 ray=normalize(vec3(mix(view.fov.x,view.fov.y,uv.x),mix(view.fov.z,view.fov.w,uv.y),-1.));
    vec3 c=vec3(dot(calibration.camera[base+2].xyz,ray),dot(calibration.camera[base+3].xyz,ray),dot(calibration.camera[base+4].xyz,ray));
    if(c.z>=-.0001){colour=vec4(0.,0.,0.,1.);return;}
    float r=length(c.xy),theta=atan(r,-c.z),t2=theta*theta;
    vec4 k=calibration.camera[base+1],intrinsics=calibration.camera[base];
    float distorted=theta*(1.+t2*(k.x+t2*(k.y+t2*(k.z+t2*k.w))));
    vec2 pixel=intrinsics.zw+vec2(c.x,-c.y)*distorted/max(r,.000001)*intrinsics.xy;
    if(any(lessThan(pixel,vec2(.5)))||any(greaterThanEqual(pixel,vec2(963.5,543.5)))){colour=vec4(0.,0.,0.,1.);return;}
    vec2 sample_uv=vec2((pixel.x+float(eye)*964.)/1928.,pixel.y/544.);
    colour=vec4(texture(passthrough,sample_uv).rgb*calibration.camera[10].xyz,1.);return;
}
vec3 d=normalize(vec3(mix(view.fov.x,view.fov.y,uv.x),mix(view.fov.z,view.fov.w,uv.y),-1.));vec4 q=view.orientation;d=normalize(d+2.*cross(q.xyz,cross(q.xyz,d)+q.w*d));d=normalize(vec3(view.environment.x*d.x-view.environment.y*d.z,d.y,view.environment.y*d.x+view.environment.x*d.z));colour=vec4(texture(panorama,vec2(atan(d.x,-d.z)/(2.*PI)+.5,.5-asin(clamp(d.y,-1.,1.))/PI)).rgb,1.);}
