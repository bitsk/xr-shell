// SPDX-License-Identifier: GPL-3.0-or-later
#version 450
struct Item{vec4 centre;vec4 right_width;vec4 down_height;vec4 uvrect;vec4 logical;vec4 extra;};
layout(set=0,binding=2,std430) readonly buffer Draws{Item items[];};
layout(push_constant) uniform View{vec4 fov;vec4 orientation;vec4 environment;vec4 origin;}view;
layout(location=0) out vec2 local_uv;
layout(location=1) flat out int item_index;
vec3 rotate_q(vec4 q,vec3 p){return p+2.0*cross(q.xyz,cross(q.xyz,p)+q.w*p);}
void main(){
 int segment=gl_VertexIndex/6;int corner=gl_VertexIndex%6;
 vec2 corners[6]=vec2[](vec2(0,0),vec2(1,0),vec2(1,1),vec2(0,0),vec2(1,1),vec2(0,1));
 vec2 uv=vec2((float(segment)+corners[corner].x)/32.0,corners[corner].y);
 Item p=items[gl_InstanceIndex];vec3 normal=cross(p.down_height.xyz,p.right_width.xyz);
 float x=(uv.x-.5)*p.right_width.w;float z=0.;
 if(p.extra.y>0.){float a=x/p.extra.y;z=p.extra.y*(1.-cos(a));x=p.extra.y*sin(a);}
 vec3 world=p.centre.xyz+p.right_width.xyz*x+p.down_height.xyz*((uv.y-.5)*p.down_height.w)+normal*z;
 vec4 inverse_q=vec4(-view.orientation.xyz,view.orientation.w);vec3 eye=rotate_q(inverse_q,world-view.origin.xyz);
 float depth=-eye.z;float ndcx=(2.*eye.x/depth-view.fov.y-view.fov.x)/(view.fov.y-view.fov.x);
 float ndcy=(-2.*eye.y/depth+view.fov.z+view.fov.w)/(view.fov.z-view.fov.w);
 gl_Position=vec4(ndcx*depth,ndcy*depth,(100./99.95)*depth-(5./99.95),depth);
 local_uv=uv;item_index=gl_InstanceIndex;
}
