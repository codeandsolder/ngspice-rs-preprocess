"""Native HiSIM-HV2 depletion drain endpoint oracle, including postsolve charge smoothing.

Extract the original coupled Poisson iteration and initialization from ngspice 47.
"""
from hisim_hv_depletion_endpoint_reference import stable_transport
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
    code += "\nif (!flg_conv) { puts(\"0\"); continue; }\n" + dep[dep.index("      /* caluculate derivative */"):dep.index("Qn_delta_dT =  cnst0_dT") + len("Qn_delta_dT =  cnst0_dT * T5 + here->HSMHV2_cnst0 * T5_dT ;")]
    start=dep.index("       if( Vds <= 0.0e0 )")
    end=dep.index("         if( flg_conv == 0 )",start)
    # Native drain loop starts after the source charge solve. Supply its
    # already-conditioned effective quasi-Fermi potential directly.
    code += "\n" + dep[start:end]
    # Retain the native converged raw endpoint even if its direct
    # exponential remainder loses the small postsolve transport density.
    code += "\nif (!flg_conv) { puts(\"0\"); continue; }\n"
    code += dep[dep.index("/* caluculate derivative */",end):dep.rfind("/*",end,dep.index("* Assign Pds.",end))]
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
 double Vgp,Vbscl,beta,Cox,nd,ns,ni,thickness,Vds;
 while(scanf("%lf %lf %lf %lf %lf %lf %lf %lf %lf",&Vgp,&Vbscl,&beta,&Cox,&nd,&ns,&ni,&thickness,&Vds)==9) {{
 struct Instance instance={{log(nd*ns/(ni*ni))/beta,nd,ns,sqrt(2*C_ESI*C_QE*nd/beta),pow(ni/nd,2),ni,2*log(nd/ni)/beta}};
 struct Model m={{thickness}}; struct Instance *here=&instance; struct Model *model=&m;
 double q_Nsub,T0,T1,T2,T3,T4,T5,T6,T7,T8,T9,T10,T11,TMF0,TMF1,TMF2;
 double Vdseff_dVgs=0,Vdseff_dVds=1,Vdseff_dVbs=0,Vdseff_dT=0;
 double Vbscl_dVbs=1,Vbscl_dT=0,Vbipn_dT=0,beta_dT=0,cnst0_dT=0,cnst1_dT=0;
 double Cox_dVg=0,Cox_dVd=0,Cox_dVb=0,Cox_dT=0,Vgp_dVgs=1,Vgp_dVds=0,Vgp_dVbs=0,Vgp_dT=0,T1_dT,T2_dT,T3_dT,T4_dT,T5_dT,T6_dT;
 const double ps_conv=1e-12,ps_conv2=1e-10,epsm10=10*DBL_EPSILON;
 const int lp_se_max=150,lp_s0_max=20;
 int lp_s0,flg_conv;
 {code}
 printf("%d %d %.17g %.17g %.17g %.17g %.17g %.17g %.17g %.17g %.17g %.17g\\n",flg_conv,depmode,phi_sL_DEP,phi_bL_DEP,phi_jL_DEP,Q_sL,Q_bL_dep,Q_subL_dep,Q_nL,Q_subL,Q_sL_dep,Q_nL_cur);
 }}
}}
"""


def main():
    root, output=Path(sys.argv[1]), Path(sys.argv[2])
    cases=list(product((-6.,-2.,-.3,0.,.5,2.,5.,8.),(-.1,-.8,-3.),(27.,44.),(.0015,.004),
        (5e22,1e23,3e23),(5e22,2e23),(1e16,), (1e-7,2e-7,5e-7),(.01,.3,1.,3.)))
    with tempfile.TemporaryDirectory(prefix="hisim-dep-") as tmp:
        c,exe=Path(tmp)/"oracle.c",Path(tmp)/"oracle"
        c.write_text(program(root))
        subprocess.run(["sccache","cc","-std=c11","-O2",str(c),"-lm","-o",str(exe)],check=True)
        result=subprocess.run([str(exe)],input="".join(" ".join(map(str,row))+"\n" for row in cases),text=True,capture_output=True,check=True)
        rows=result.stdout.splitlines()
        assert len(rows)==len(cases)
        valid=0
        with output.open("w") as f:
            f.write("# gate body beta cox depletion_doping substrate_doping ni thickness qf native_mode surface buried junction surface_charge buried_charge substrate_charge accumulation_charge substrate_inversion surface_depletion transport_carrier stable_transport_carrier\n")
            for case,row in zip(cases,rows):
                words=row.split()
                if words[0]!="1" or not all(math.isfinite(float(x)) for x in words[:-1]):
                    print("native rejected",case,row,file=sys.stderr)
                    continue
                valid+=1
                precise=stable_transport(case[2],case[4],words[2],case[8])
                f.write(" ".join(map(str,case))+" "+" ".join(words[1:])+f" {precise:.20g}\n")
        print(f"{valid}/{len(cases)} native converged cases")
if __name__=="__main__":main()
