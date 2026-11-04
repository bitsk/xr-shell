// SPDX-License-Identifier: GPL-3.0-or-later
#version 450
layout(location=0) in vec3 position;
layout(location=1) in vec3 normal;
layout(location=2) in vec2 uv;
layout(push_constant) uniform View {vec4 fov;vec4 orientation;vec4 origin;vec4 model_q;vec4 model_p;vec4 controls;}view;
layout(location=0) out vec2 texture_uv;
layout(location=1) out vec3 world_normal;
vec3 rotate_q(vec4 q,vec3 p){return p+2.*cross(q.xyz,cross(q.xyz,p)+q.w*p);}
void main(){
 vec3 world=rotate_q(view.model_q,position)+view.model_p.xyz;
 vec3 eye=rotate_q(vec4(-view.orientation.xyz,view.orientation.w),world-view.origin.xyz);
 float depth=-eye.z;
 float x=(2.*eye.x/depth-view.fov.y-view.fov.x)/(view.fov.y-view.fov.x);
 float y=(-2.*eye.y/depth+view.fov.z+view.fov.w)/(view.fov.z-view.fov.w);
 gl_Position=vec4(x*depth,y*depth,(100./99.95)*depth-(5./99.95),depth);
 texture_uv=uv;world_normal=rotate_q(view.model_q,normal);
}
