"""Native HiSIM-HV2 depletion source endpoint oracle, before postsolve charge smoothing.

Extract the original coupled Poisson iteration and initialization from ngspice 47.
"""
from pathlib import Path
from itertools import product
import math
import subprocess
import sys
import tempfile

REVISION = "972dbf45db1e47d18bc4fda5d8bb829aff2d0e05"

def program(root):
    def source(name):
        return subprocess.check_output(["git", "-C", str(root), "show",
            f"{REVISION}:src/spicelib/devices/hisimhv2/{name}"], text=True)
    dep = source("hsmhv2eval_dep.h")
    eval_c = source("hsmhv2eval.c")
    macros = eval_c[eval_c.index("#define Fn_CP("):eval_c.index("/*---------------------------------------------------*", eval_c.index("#define Fn_SL_CP("))]
    code = dep[dep.index("/* define local variavles */"):dep.index("       if( flg_conv == 0 )")]
    return f"""#include <math.h>
#include <stdio.h>
#include <float.h>
#define C_QE 1.6021918e-19
#define C_ESI 1.034943e-10
#define Fn_Pow pow
#define Fn_Sgn(x) ((x)>0?1.:((x)<0?-1.:0.))
{macros}
struct Instance {{double HSMHV2_Vbipn,HSMHV2_ndepm,HSMHV2_nsub,HSMHV2_cnst0,HSMHV2_cnst1,HSMHV2_nin,HSMHV2_Pb2n;}};
struct Model {{double HSMHV2_tndep;}};
int main(void) {{
 double Vgp,Vbscl,beta,Cox,nd,ns,ni,thickness;
 while(scanf("%lf %lf %lf %lf %lf %lf %lf %lf",&Vgp,&Vbscl,&beta,&Cox,&nd,&ns,&ni,&thickness)==8) {{
 struct Instance instance={{log(nd*ns/(ni*ni))/beta,nd,ns,sqrt(2*C_ESI*C_QE*nd/beta),pow(ni/nd,2),ni,2*log(nd/ni)/beta}};
 struct Model m={{thickness}}; struct Instance *here=&instance; struct Model *model=&m;
 double q_Nsub,T0,T1,T2,T3,T4,T5,T7,T8,T9,T10,TMF0,TMF1,TMF2;
 const double ps_conv=1e-12,ps_conv2=1e-10,epsm10=10*DBL_EPSILON;
 const int lp_se_max=150,lp_s0_max=20;
 int lp_s0,flg_conv;
 {code}
 printf("%d %d %.17g %.17g %.17g %.17g %.17g %.17g\\n",flg_conv,depmode,phi_s0_DEP,phi_b0_DEP,phi_j0_DEP,Q_s0,Q_b0_dep,Q_sub0_dep);
 }}
}}
"""

def main():
    root, output=Path(sys.argv[1]), Path(sys.argv[2])
    cases=list(product((-8.,-4.,-2.,-1.,-.3,0.,.3,1.,3.,8.),(-.1,-.8,-3.),(27.,38.,44.),(.0015,.004),
        (5e22,1e23,3e23),(5e22,2e23),(1e16,), (1e-7,2e-7,5e-7)))
    with tempfile.TemporaryDirectory(prefix="hisim-dep-") as tmp:
        c,exe=Path(tmp)/"oracle.c",Path(tmp)/"oracle"
        c.write_text(program(root))
        subprocess.run(["sccache","cc","-std=c11","-O2",str(c),"-lm","-o",str(exe)],check=True)
        result=subprocess.run([str(exe)],input="".join(" ".join(map(str,row))+"\n" for row in cases),text=True,capture_output=True,check=True)
        rows=result.stdout.splitlines()
        assert len(rows)==len(cases)
        valid=0
        with output.open("w") as f:
            f.write("# gate body beta cox depletion_doping substrate_doping ni thickness native_mode source_potential buried_potential junction_potential surface_charge buried_charge substrate_charge\n")
            for case,row in zip(cases,rows):
                words=row.split()
                if words[0]!="1" or not all(math.isfinite(float(x)) for x in words): continue
                valid+=1
                f.write(" ".join(map(str,case))+" "+" ".join(words[1:])+"\n")
        print(f"{valid}/{len(cases)} native converged cases")
if __name__=="__main__":main()
