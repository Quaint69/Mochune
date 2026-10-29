uniform sampler2D previous_frame;
uniform sampler2D next_frame;
uniform vec2 image_size;
uniform sampler2D previous_preview;
uniform sampler2D next_preview;
in vec2 uv;
out vec4 result;
float luma(vec3 c) { return dot(c, vec3(0.2126, 0.7152, 0.0722)); }
vec2 error_at(vec2 f, vec2 b, float radius) {
    vec2 e=vec2(0);
    for(int p=0;p<5;++p){
        vec2 o=p==0?vec2(0):p==1?vec2(-radius,0):p==2?vec2(radius,0):p==3?vec2(0,-radius):vec2(0,radius);
        vec2 at=uv+o/image_size;
        float a=luma(texture(previous_frame,at).rgb),z=luma(texture(next_frame,at).rgb);
        float df=a-luma(texture(next_frame,at+f/image_size).rgb);
        float db=z-luma(texture(previous_frame,at+b/image_size).rgb);
        e+=vec2(df*df,db*db);
    }
    return e/5.0+vec2(dot(f,f),dot(b,b))*0.00000002;
}
// Search a blurred coarse pyramid first so large pans cannot get trapped
// matching unrelated high-frequency details. Distances here are preview pixels.
vec2 coarse_error(vec2 f,vec2 b){
    vec2 size=vec2(textureSize(previous_preview,0)),e=vec2(0);
    for(int p=0;p<9;++p){
        vec2 o=vec2(float(p%3-1),float(p/3-1))*3.0;
        vec2 at=uv+o/size;
        vec3 a=texture(previous_preview,at).rgb,z=texture(next_preview,at).rgb;
        vec3 df=a-texture(next_preview,at+f/size).rgb;
        vec3 db=z-texture(previous_preview,at+b/size).rgb;
        e+=vec2(dot(df,df),dot(db,db));
    }
    return e/27.0+vec2(dot(f,f),dot(b,b))*0.000001;
}
// A motion estimate needs a spatial feature. Eight-pixel-wide context
// covers the coarse grid cell, including thin edges between sample centers.
float spatial_contrast(){
    vec3 a=texture(previous_frame,uv).rgb,z=texture(next_frame,uv).rgb;
    float contrast=0.0;
    for(int p=0;p<4;++p){
        vec2 o=p==0?vec2(-4,0):p==1?vec2(4,0):p==2?vec2(0,-4):vec2(0,4);
        vec3 da=texture(previous_frame,uv+o/image_size).rgb-a;
        vec3 dz=texture(next_frame,uv+o/image_size).rgb-z;
        contrast=max(contrast,max(dot(da,da),dot(dz,dz)));
    }
    return contrast;
}

// Search-only scores: discarded candidates need not finish their patch. Keep
// complete scores in error_at/coarse_error for confidence/context decisions.
vec2 error_search(vec2 f,vec2 b,float radius,vec2 bound){
    vec2 penalty=vec2(dot(f,f),dot(b,b))*0.00000002;
    if(all(greaterThanEqual(penalty,bound)))return max(penalty,bound);
    vec2 e=vec2(0);
    for(int p=0;p<5;++p){
        vec2 o=p==0?vec2(0):p==1?vec2(-radius,0):p==2?vec2(radius,0):p==3?vec2(0,-radius):vec2(0,radius);
        vec2 at=uv+o/image_size;
        float a=luma(texture(previous_frame,at).rgb),z=luma(texture(next_frame,at).rgb);
        float df=a-luma(texture(next_frame,at+f/image_size).rgb);
        float db=z-luma(texture(previous_frame,at+b/image_size).rgb);
        e+=vec2(df*df,db*db);
        vec2 score=e/5.0+penalty;
        // Squared-error terms are nonnegative. Neither direction can win now.
        if(all(greaterThanEqual(score,bound)))return max(score,bound);
    }
    return e/5.0+penalty;
}
vec3 patch_a[9];
vec3 patch_b[9];
void cache_patch(){
 vec2 size=vec2(textureSize(previous_preview,0));
 for(int p=0;p<9;++p){vec2 at=uv+vec2(float(p%3-1),float(p/3-1))*3.0/size;patch_a[p]=texture(previous_preview,at).rgb;patch_b[p]=texture(next_preview,at).rgb;}
}
vec2 coarse_search(vec2 f,vec2 b,vec2 bound){
    vec2 size=vec2(textureSize(previous_preview,0)),e=vec2(0);
    vec2 penalty=vec2(dot(f,f),dot(b,b))*0.000001;
    if(all(greaterThanEqual(penalty,bound)))return max(penalty,bound);
    for(int p=0;p<9;++p){
        vec2 o=vec2(float(p%3-1),float(p/3-1))*3.0;
        vec2 at=uv+o/size;
        vec3 a=patch_a[p],z=patch_b[p];
        vec3 df=a-texture(next_preview,at+f/size).rgb;
        vec3 db=z-texture(previous_preview,at+b/size).rgb;
        e+=vec2(dot(df,df),dot(db,db));
        vec2 score=e/27.0+penalty;
        if(all(greaterThanEqual(score,bound)))return max(score,bound);
    }
    return e/27.0+penalty;
}

void main(){
    float contrast=spatial_contrast();
    if(contrast<0.0001){result=vec4(0);return;}
    vec2 cell=1.0/vec2(textureSize(previous_preview,0));
    vec3 ca=texture(previous_preview,uv).rgb,cb=texture(next_preview,uv).rgb;
    float edge_context=0.0;
    for(int p=0;p<4;++p){
        vec2 o=p==0?vec2(-1,0):p==1?vec2(1,0):p==2?vec2(0,-1):vec2(0,1);
        vec3 da=texture(previous_preview,uv+o*cell).rgb-ca;
        vec3 db=texture(next_preview,uv+o*cell).rgb-cb;
        edge_context=max(edge_context,max(dot(da,da),dot(db,db)));
    }
    bool sharp=contrast>0.2 || edge_context>0.08;
    vec2 f=vec2(0),b=vec2(0),best=error_at(f,b,2.0);
    // Preserve the inexpensive local search for well-matched small movement.
    if(max(best.x,best.y)>0.000025){
        for(int level=0;level<4;++level){
            float step_size=level==0?8.0:level==1?4.0:level==2?2.0:1.0;
            vec2 of=f,ob=b;
            for(int y=-1;y<=1;++y)for(int x=-1;x<=1;++x){
                vec2 o=vec2(float(x),float(y))*step_size;
                vec2 cf=of+o,cb=ob+o,e=error_search(cf,cb,2.0,best);
                if(e.x<best.x){best.x=e.x;f=cf;}
                if(e.y<best.y){best.y=e.y;b=cb;}
            }
        }
        // Poor or context-inconsistent matches get the bounded coarse search.
        // Wider context at coarse scales avoids matching a flat edge fragment.
        vec2 preview_scale=image_size/vec2(textureSize(previous_preview,0));
        vec2 context_error=coarse_error(f/preview_scale,b/preview_scale);
        vec2 hint_f=f,hint_b=b;
        if(max(best.x,best.y)>0.0001 || max(context_error.x,context_error.y)>0.0001){
            cache_patch();
            vec2 wf=vec2(0),wb=vec2(0),wide_best=coarse_error(vec2(0),vec2(0));
            for(int y=-8;y<=8;++y)for(int x=-8;x<=8;++x){
                vec2 c=vec2(float(x),float(y)),e=coarse_search(c,c,wide_best);
                if(e.x<wide_best.x){wide_best.x=e.x;wf=c;}
                if(e.y<wide_best.y){wide_best.y=e.y;wb=c;}
            }
            // Subpixel refinement in the blurred pyramid, then native pixels.
            for(int level=0;level<2;++level){
                float step_size=level==0?0.5:0.25;
                vec2 of=wf,ob=wb;
                for(int y=-1;y<=1;++y)for(int x=-1;x<=1;++x){
                    vec2 o=vec2(float(x),float(y))*step_size;
                    vec2 cf=of+o,cb=ob+o,e=coarse_search(cf,cb,wide_best);
                    if(e.x<wide_best.x){wide_best.x=e.x;wf=cf;}
                    if(e.y<wide_best.y){wide_best.y=e.y;wb=cb;}
                }
            }
            vec2 scale=image_size/vec2(textureSize(previous_preview,0));
            wf*=scale;wb*=scale;
            vec2 wbest=error_at(wf,wb,2.0);
            for(int level=0;level<4;++level){
                float step_size=level==0?4.0:level==1?2.0:level==2?1.0:0.5;
                vec2 of=wf,ob=wb;
                for(int y=-1;y<=1;++y)for(int x=-1;x<=1;++x){
                    vec2 o=vec2(float(x),float(y))*step_size;
                    vec2 cf=of+o,cb=ob+o,e=error_search(cf,cb,2.0,wbest);
                    if(e.x<wbest.x){wbest.x=e.x;wf=cf;}
                    if(e.y<wbest.y){wbest.y=e.y;wb=cb;}
                }
            }
            vec2 e=error_at(wf,wb,2.0);
            hint_f=wf;hint_b=wb;
            vec2 guide=coarse_error(wf/scale,wb/scale);
            // A broad context can disambiguate identical pixels along a large
            // edge, but must not override a sharp match on a rotating outline.
            if((!sharp && e.x<best.x) || (e.x<best.x+0.0002 && guide.x<0.00002 && guide.x<context_error.x*0.35))f=wf;
            if((!sharp && e.y<best.y) || (e.y<best.y+0.0002 && guide.y<0.00002 && guide.y<context_error.y*0.35))b=wb;
        }
        // Preserve accurate thin high-contrast silhouettes with a native
        // search; blurred context is primarily for textured sliding surfaces.
        best=error_at(f,b,2.0);
        vec2 final_context=coarse_error(f/preview_scale,b/preview_scale);
        if(sharp && max(best.x,best.y)>0.0005 && max(final_context.x,final_context.y)>0.0002){
            vec2 wf=hint_f,wb=hint_b,wide_best=error_at(hint_f,hint_b,4.0);
            // Correct native edge phase around the pyramid proposal instead
            // of repeating the whole displacement search.
            for(int y=-8;y<=8;++y)for(int x=-8;x<=8;++x){
                vec2 offset=vec2(float(x),float(y))*2.0;
                vec2 cf=hint_f+offset,cb=hint_b+offset,e=error_search(cf,cb,4.0,wide_best);
                if(e.x<wide_best.x){wide_best.x=e.x;wf=cf;}
                if(e.y<wide_best.y){wide_best.y=e.y;wb=cb;}
            }
            for(int level=0;level<2;++level){
                float step_size=level==0?2.0:1.0;
                vec2 wbest=error_at(wf,wb,2.0),of=wf,ob=wb;
                for(int y=-1;y<=1;++y)for(int x=-1;x<=1;++x){
                    vec2 o=vec2(float(x),float(y))*step_size;
                    vec2 cf=of+o,cb=ob+o,e=error_search(cf,cb,2.0,wbest);
                    if(e.x<wbest.x){wbest.x=e.x;wf=cf;}
                    if(e.y<wbest.y){wbest.y=e.y;wb=cb;}
                }
            }
            vec2 e=error_at(wf,wb,2.0);
            if(e.x<best.x)f=wf;
            if(e.y<best.y)b=wb;
            best=error_at(f,b,2.0);
        }

    }
    result=vec4(f/image_size,b/image_size);
}
