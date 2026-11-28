"""Independent HiSIM-HV Poisson residual and root oracle from ngspice 47.

Usage: uv run --no-project python hisim_hv_surface_reference.py NGSPICE_CHECKOUT OUTPUT
Extract the native D1/D2/D3 source and drain Poisson evaluation branches.
The root is independently recovered by bracketed C bisection; no Rust result
or differentiated iteration is used. The C branch uses the native polynomials.
"""
from itertools import product
from pathlib import Path
import math
import re
import subprocess
import sys
import tempfile

REVISION = "972dbf45db1e47d18bc4fda5d8bb829aff2d0e05"


def program(root, version):
    prefix = "hsmhv" if version == 1 else "hsmhv2"
    path = f"src/spicelib/devices/hisimhv{version}/{prefix}eval.c"
    c = subprocess.check_output(["git", "-C", str(root), "show", f"{REVISION}:{path}"], text=True)
    constants = "\n".join(re.findall(r"const double (?:cn_nc5[1-5]|cn_im5[3-5]|znbd5|large_arg)\s*=\s*[^;]+;", c))
    def extract(loop, equation):
        start = c.index("Chi = beta * (", c.index(f"for ( {loop} = 1"))
        end = c.index(f"{equation} = Vgp", start)
        return c[start:end]
    source = extract("lp_s0", "Fs0")
    drain = extract("lp_sl", "Fsl")
    source_code = f'''#include <math.h>
#include <stdio.h>
{constants}
static void charge(int drain, double psi, double body, double qf, double beta, double cnst1, double *out) {{
 double Ps0=psi,Psl=psi,Vbscl=body,Vds=qf,Chi=0,fi=0,fi_dChi=0,fb=0,fb_dChi=0;
 double exp_bVbs=exp(beta*body),exp_bVbsVds=exp(beta*(body-qf)),cfs1=cnst1*exp_bVbs;
 double exp_Chi=0,exp_bPs0=0,Rho=0,exp_Rho=0,Xil=0;
 double fs01=0,fs02=0,fs01_dPs0=0,fs02_dPs0=0,fsl1=0,fsl2=0,fsl1_dPsl=0,fsl2_dPsl=0;
 if (drain) {{ {drain} out[3]=fsl1;out[4]=fsl2; }}
 else {{ {source} out[3]=fs01;out[4]=fs02; }}
 out[0]=psi;out[1]=Chi;out[2]=(Chi<znbd5?fb:sqrt(Chi-1.));
}}
static void solve(int drain,double gate,double body,double qf,double beta,double factor,double cnst1,double*out) {{
 double low=body,high=gate;
 for(int i=0;i<100;i++){{ double mid=low+(high-low)*.5;charge(drain,mid,body,qf,beta,cnst1,out);
 if(gate-mid-factor*out[4]>0) low=mid; else high=mid; }}
 charge(drain,low+(high-low)*.5,body,qf,beta,cnst1,out);
}}
int main(void){{ int mode;double chi,body,qf,beta,factor,cnst1;
 while(scanf("%d %lf %lf %lf %lf %lf %lf",&mode,&chi,&body,&qf,&beta,&factor,&cnst1)==7){{
 double out[5];charge(mode,body+chi/beta,body,qf,beta,cnst1,out);
 double gate=out[0]+factor*out[4];solve(mode,gate,body,qf,beta,factor,cnst1,out);
 printf("%.17g",gate);for(int i=0;i<5;i++)printf(" %.17g",out[i]);
 // Independent finite differences of converged native roots and charges.
 double grad[6][4]={{{{1.,.04,-.03,.0002}},{{0.,.015,.91,-.00015}},{{0.,.8,.02,0.}},{{0.,0.,0.,-.13}},{{.001,.0008,.0012,.00003}},{{0.,0.,0.,cnst1*.025}}}};
 if (!mode) for(int j=0;j<4;j++)grad[2][j]=0.;
 double args[6]={{gate,body,qf,beta,factor,cnst1}};
 for(int axis=0;axis<4;axis++){{ double hi[6],lo[6],a[5],b[5],step=axis==3?1e-3:1e-5;
 for(int j=0;j<6;j++){{hi[j]=args[j]+grad[j][axis]*step;lo[j]=args[j]-grad[j][axis]*step;}}
 solve(mode,hi[0],hi[1],hi[2],hi[3],hi[4],hi[5],a);solve(mode,lo[0],lo[1],lo[2],lo[3],lo[4],lo[5],b);
 for(int j=0;j<5;j++)printf(" %.17g",(a[j]-b[j])/(2*step)); }}
 putchar('\\n');
 }} }}
'''
    # Extended precision is used only by the independent root/finite-difference
    # oracle, avoiding catastrophic cancellation for deliberately extreme
    # inversion densities. The extracted native equations are unchanged.
    return source_code.replace('double', 'long double').replace('%lf', '%Lf').replace('%.17g', '%.21Lg').replace('exp(', 'expl(').replace('sqrt(', 'sqrtl(')


def main():
    root, output = Path(sys.argv[1]), Path(sys.argv[2])
    # Wide enough for the native large-Chi branch, but physically moderate
    # surface potentials. Include points around all native region boundaries.
    cases = [(mode, chi, body, qf, beta, factor, carrier)
             for mode, chi, body, beta, factor, carrier in product(
                 (0,1),(0.001,.1,2.9,3.1,4.99,5.01,12.,40.,79.,81.),(-.4,-1.7),(27.,44.),(.09,.7),(1e-16,3e-13))
             for qf in ((0.,) if not mode else (.01,.9))]
    with tempfile.TemporaryDirectory(prefix="hisim-surface-oracle-") as temp, output.open("w") as f:
        f.write("# family drain chi_seed body qf beta fac1 cnst1 gate; psi chi depletion inversion_squared total; four groups of five finite-difference partials\n")
        for version in (1,2):
            c, exe = Path(temp)/"oracle.c", Path(temp)/"oracle"
            c.write_text(program(root,version))
            subprocess.run(["sccache","cc","-std=c11","-O2",str(c),"-lm","-o",str(exe)],check=True)
            result = subprocess.run([str(exe)],input="".join(" ".join(map(str,row))+"\n" for row in cases),text=True,capture_output=True,check=True)
            rows=result.stdout.splitlines()
            assert len(rows)==len(cases)
            for case,row in zip(cases,rows):
                assert all(math.isfinite(float(x)) for x in row.split()),(case,row)
                f.write(" ".join(map(str,(version,*case)))+" "+row+"\n")

if __name__=="__main__":main()
