// SPDX-License-Identifier: GPL-3.0-or-later
#version 450
struct Item{vec4 centre;vec4 right_width;vec4 down_height;vec4 uvrect;vec4 logical;vec4 extra;};
layout(set=0,binding=2,std430) readonly buffer Draws{Item items[];};
layout(push_constant) uniform View{vec4 fov;vec4 orientation;vec4 environment;vec4 origin;}view;
layout(location=0) out vec2 local_uv;
layout(location=1) flat out int item_index;
layout(location=2) flat out int side_face;
layout(location=3) out vec3 world_position;
layout(location=4) out vec3 world_normal;
const float PI=3.141592653589793;
vec3 rotate_q(vec4 q,vec3 p){return p+2.0*cross(q.xyz,cross(q.xyz,p)+q.w*p);}
vec2 perimeter(int edge,float endpoint,vec2 size,float r){
 int quadrant=edge/9, step=edge%9;
 vec2 centres[4]=vec2[](vec2(1,-1),vec2(1,1),vec2(-1,1),vec2(-1,-1));
 vec2 centre=centres[quadrant]*(size*.5-r);
 float angle=float(quadrant)*PI*.5-PI*.5;
 if(step<8){angle+=(float(step)+endpoint)*PI/16.;return centre+r*vec2(cos(angle),sin(angle));}
 if(endpoint<.5){angle+=PI*.5;return centre+r*vec2(cos(angle),sin(angle));}
 quadrant=(quadrant+1)%4;centre=centres[quadrant]*(size*.5-r);angle=float(quadrant)*PI*.5-PI*.5;
 return centre+r*vec2(cos(angle),sin(angle));
}
void main(){
 Item p=items[gl_InstanceIndex];item_index=gl_InstanceIndex;side_face=0;
 vec2 corners[6]=vec2[](vec2(0,0),vec2(1,0),vec2(1,1),vec2(0,0),vec2(1,1),vec2(0,1));
 vec2 uv;float inset=0.;vec2 edge_normal=vec2(0);
 if(gl_VertexIndex<192){int segment=gl_VertexIndex/6;uv=vec2((float(segment)+corners[gl_VertexIndex%6].x)/32.,corners[gl_VertexIndex%6].y);}
 else{
  side_face=1;float thickness=p.extra.z;
  if(thickness<=0.){local_uv=vec2(0);world_position=p.centre.xyz;world_normal=vec3(0,0,1);gl_Position=vec4(2,2,0,1);return;}
  int edge=(gl_VertexIndex-192)/6;vec2 c=corners[(gl_VertexIndex-192)%6];float r=min(p.logical.z,min(p.logical.x,p.logical.y)*.5);
  vec2 a=perimeter(edge,0.,p.logical.xy,r),b=perimeter(edge,1.,p.logical.xy,r);
  uv=mix(a,b,c.x)/p.logical.xy+.5;inset=-c.y*thickness;
  vec2 tangent=normalize(b-a);edge_normal=vec2(tangent.y,-tangent.x);
 }
 vec3 n=cross(p.down_height.xyz,p.right_width.xyz);float x=(uv.x-.5)*p.right_width.w;float z=0.;vec3 tangent=p.right_width.xyz,normal=n;
 if(p.extra.y>0.){float a=x/p.extra.y;z=p.extra.y*(1.-cos(a));x=p.extra.y*sin(a);tangent=p.right_width.xyz*cos(a)+n*sin(a);normal=n*cos(a)-p.right_width.xyz*sin(a);}
 vec3 world=p.centre.xyz+p.right_width.xyz*x+p.down_height.xyz*((uv.y-.5)*p.down_height.w)+n*z+normal*inset;
 world_position=world;world_normal=side_face==1?normalize(tangent*edge_normal.x+p.down_height.xyz*edge_normal.y):normal;
 vec4 inverse_q=vec4(-view.orientation.xyz,view.orientation.w);vec3 eye=rotate_q(inverse_q,world-view.origin.xyz);
 float depth=-eye.z;float ndcx=(2.*eye.x/depth-view.fov.y-view.fov.x)/(view.fov.y-view.fov.x);
 float ndcy=(-2.*eye.y/depth+view.fov.z+view.fov.w)/(view.fov.z-view.fov.w);
 gl_Position=vec4(ndcx*depth,ndcy*depth,(100./99.95)*depth-(5./99.95),depth);local_uv=uv;
}
