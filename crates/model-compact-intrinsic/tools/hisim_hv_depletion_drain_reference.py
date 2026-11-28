"""Source-extracted HiSIM-HV2 depletion effective channel and drift biases."""
from itertools import product
from pathlib import Path
import math
import subprocess
import sys
import tempfile

REVISION="972dbf45db1e47d18bc4fda5d8bb829aff2d0e05"

def program(root):
    def source(name):
        return subprocess.check_output(["git","-C",str(root),"show",f"{REVISION}:src/spicelib/devices/hisimhv2/{name}"],text=True)
    dep=source("hsmhv2eval_dep.h")
    eval_c=source("hsmhv2eval.c")
    macros=eval_c[eval_c.index("#define Fn_CP("):eval_c.index("/*---------------------------------------------------*",eval_c.index("#define Fn_SL_CP("))]
    channel=dep[dep.index("       Vdsorg = Vds ;"):dep.index("       /* Vdseff (end) */")]
    drift=dep[dep.index("       if( Vdsorg > 1e-3 )"):dep.index("       T0 = here->HSMHV2_ninvd",dep.index("       if( Vdsorg > 1e-3 )"))]
    return f"""#include <math.h>
#include <stdio.h>
#include <float.h>
#define C_QE 1.6021918e-19
#define C_ESI 1.034943e-10
#define Fn_Pow pow
{macros}
struct Instance {{double HSMHV2_qnsub_esi,HSMHV2_ddlt;}};
struct Model {{double HSMHV2_depvdsef1,HSMHV2_depvdsef2,HSMHV2_depleak;}};
int main(void) {{
 double Vgp,Vbsz,Vds,beta_inv,Cox,nd,ddlt,off,scale,floor;
 while(scanf("%lf %lf %lf %lf %lf %lf %lf %lf %lf %lf",&Vgp,&Vbsz,&Vds,&beta_inv,&Cox,&nd,&ddlt,&off,&scale,&floor)==10) {{
 const double epsm10=10*DBL_EPSILON,small=1e-25,DEPQFN3=0.3,DEPQFN_dlt=2.0;
 double q_Ndepm_esi=C_QE*C_ESI*nd;
 struct Instance inst={{q_Ndepm_esi,ddlt}};struct Instance *here=&inst;
 struct Model m={{off,scale,floor}};struct Model *model=&m;
 double T0,T1,T2,T3,T4,T5,T6,T7,T9,T10,TMF0,TMF1,TMF2;
 double Vdsorg,Vdseff,Vdseff0,Vgpp,Vgpp_dVgs,Vgpp_dVds,Vgpp_dVbs,Vgpp_dT;
 double Vdseff_dVgs,Vdseff_dVds,Vdseff_dVbs,Vdseff_dT,Vdseff0_dVgs,Vdseff0_dVds,Vdseff0_dVbs,Vdseff0_dT;
 double T0_dVg,T0_dVd,T0_dVb,T0_dT,T1_dVg,T1_dVd,T1_dVb,T1_dT;
 double T2_dVg,T2_dVd,T2_dVb,T2_dT,T3_dVg,T3_dVd,T3_dVb,T3_dT;
 double T4_dVg,T4_dVd,T4_dVb,T4_dT,T6_dVg,T6_dVd,T6_dVb,T6_dT;
 double T7_dVg,T7_dVd,T7_dVb,T7_dT,T9_dVg,T9_dVd,T9_dVb,T9_dT,T10_dVg,T10_dVd,T10_dVb,T10_dT;
 double Cox_dVg=0,Cox_dVd=0,Cox_dVb=0,Cox_dT=0,Vgp_dVgs=1,Vgp_dVds=0,Vgp_dVbs=0,Vgp_dT=0,Vbsz_dVds=0,Vbsz_dVbs=1,Vbsz_dT=0,beta_inv_dT=0;
 Vgp += epsm10*1e7;
 {channel}
 {drift}
 printf("%.17g %.17g\\n",Vds,Vdseff0);
 }}
}}
"""

def main():
    root,output=Path(sys.argv[1]),Path(sys.argv[2])
    cases=[(*x,*card) for x in product((-8.,-3.,-1.,-.1,0.,.1,1.,3.,8.),(-.1,-.8,-3.),(0.,1e-5,0.001,0.00101,.02,.2,1.,5.,15.),(1/27.,1/44.),(.0015,.004),(5e22,3e23),(1.2,3.,8.)) for card in [(2.,.5,.5),(1.2,.9,.03)]]
    with tempfile.TemporaryDirectory(prefix="hisim-dep-bias-") as tmp:
        c,exe=Path(tmp)/"oracle.c",Path(tmp)/"oracle"
        c.write_text(program(root))
        subprocess.run(["sccache","cc","-std=c11","-O2",str(c),"-lm","-o",str(exe)],check=True)
        result=subprocess.run([str(exe)],input="".join(" ".join(map(str,row))+"\n" for row in cases),text=True,capture_output=True,check=True)
        rows=result.stdout.splitlines();assert len(rows)==len(cases)
        with output.open("w") as f:
            f.write("# gate symmetric_body drain thermal_voltage cox depletion_doping ddlt drift_offset drift_scale drift_floor channel_effective_drain drift_effective_drain\n")
            for case,row in zip(cases,rows):
                assert all(math.isfinite(float(x)) for x in row.split()),(case,row)
                f.write(" ".join(map(str,case))+" "+row+"\n")
        print(len(cases))
if __name__=="__main__":main()
