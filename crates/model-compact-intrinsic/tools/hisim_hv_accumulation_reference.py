"""Extract exact HiSIM-HV normal-mode accumulation values from ngspice 47.

Usage: uv run --no-project python hisim_hv_accumulation_reference.py NGSPICE_CHECKOUT OUTPUT
Central differences independently differentiate the source-extracted values.
"""
from itertools import product
from pathlib import Path
import math
import re
import subprocess
import sys
import tempfile

REVISION="972dbf45db1e47d18bc4fda5d8bb829aff2d0e05"


def program(root, version):
    prefix="HSMHV" if version==1 else "HSMHV2"
    base="hsmhv" if version==1 else "hsmhv2"
    path=f"src/spicelib/devices/hisimhv{version}/{base}eval.c"
    src=subprocess.check_output(["git","-C",str(root),"show",f"{REVISION}:{path}"],text=True)
    start=src.index("* Evaluation of Ps0.",src.index("* Accumulation zone."))
    # First active Ps0_min assignment, not the commented-out HV1 expression.
    start=re.search(r"(?m)^\s+Ps0_min =",src[start:]).start()+start
    end=src.index("Psl = Ps0",start)
    block=src[start:end]
    derivs=set(re.findall(r"\b\w+_d(?:V\w+|T)\b",block))
    variables=set("Ps0_min TX TY T1 T2 T3 T4 T5 T9 Ac41 Ac4 Ac31 Ac3 Ac2 Ac1 Acd Acn Chi Psa Ps0".split())|derivs
    decl="\n".join(f"double {name}=0.;" for name in sorted(variables))
    return f'''#include <math.h>
#include <stdio.h>
#define C_SQRT_2 1.414213562373095e+00
#define C_1o3 3.333333333333333e-01
#define Fn_Pow pow
typedef struct{{double {prefix}_eg;}} Here;
static void value(double Vgp,double Vbscl,double beta,double cnst0,double Cox,double eg,double Pb2,double Vgs_min,double*out){{
 double beta_inv=1/beta,fac1=cnst0/Cox;
 Here instance={{eg}};Here*here=&instance;
 {decl}
 {block}
 out[0]=Ps0;out[1]=Cox*(Vgp-Ps0);
}}
int main(void){{ double x[8];while(1){{
 for(int i=0;i<8;i++)if(scanf("%lf",&x[i])!=1)return 0;
 double center[2];value(x[0],x[1],x[2],x[3],x[4],x[5],x[6],x[7],center);
 printf("%.17g %.17g",center[0],center[1]);
 double grad[8][4]={{{{1.,.04,-.03,.0002}},{{0.,.015,.91,-.00015}},{{0.,0.,0.,-.13}},{{.0001,.00008,.00012,.000003}},{{.0002,.00003,.00002,0.}},{{0.,0.,0.,-.00018}},{{0.,0.,0.,.0004}},{{0.,0.,0.,0.}}}};
 for(int axis=0;axis<4;axis++){{double hi[8],lo[8],a[2],b[2],step=axis==3?1e-4:1e-6;
 for(int j=0;j<8;j++){{hi[j]=x[j]+grad[j][axis]*step;lo[j]=x[j]-grad[j][axis]*step;}}
 value(hi[0],hi[1],hi[2],hi[3],hi[4],hi[5],hi[6],hi[7],a);value(lo[0],lo[1],lo[2],lo[3],lo[4],lo[5],lo[6],lo[7],b);
 printf(" %.17g %.17g",(a[0]-b[0])/(2*step),(a[1]-b[1])/(2*step));}}
 putchar('\\n');}}}}
'''


def main():
    root,out=Path(sys.argv[1]),Path(sys.argv[2])
    cases=[(body-delta,body,beta,cnst,cox,1.14,.72,vmin) for delta,body,beta,cnst,cox,vmin in product((.0001,.03,.3,2.,12.),(-.5,-1.7),(27.,44.),(.001,.003),(.004,.01),(-10.,-50.))]
    with tempfile.TemporaryDirectory(prefix="hisim-accumulation-oracle-") as temp,out.open("w") as f:
        f.write("# family Vgp Vbscl beta cnst0 Cox Eg Pb2 Vgs_min; Ps0 Qbu; four groups of two finite-difference partials\n")
        for version in (1,2):
            c,exe=Path(temp)/"oracle.c",Path(temp)/"oracle"
            c.write_text(program(root,version))
            subprocess.run(["sccache","cc","-std=c11","-O2",str(c),"-lm","-o",str(exe)],check=True)
            result=subprocess.run([str(exe)],input="".join(" ".join(map(str,row))+"\n" for row in cases),text=True,capture_output=True,check=True)
            rows=result.stdout.splitlines()
            assert len(rows)==len(cases)
            for case,row in zip(cases,rows):
                assert all(math.isfinite(float(x)) for x in row.split()),(case,row)
                f.write(" ".join(map(str,(version,*case)))+" "+row+"\n")

if __name__=="__main__":main()
