"""Extract native HiSIM-HV effective drain and saturation smoothing values."""
from itertools import product
from pathlib import Path
import math
import re
import subprocess
import sys
import tempfile
REVISION='972dbf45db1e47d18bc4fda5d8bb829aff2d0e05'
GRADIENTS=((0,1,0,0),(1,.04,-.03,.0002),(0,.015,.91,-.00015),(0,0,0,8.617e-5),(.0002,.00003,.00002,0))
def program(root,version):
    prefix='HSMHV' if version==1 else 'HSMHV2'
    base='hsmhv' if version==1 else 'hsmhv2'
    src=subprocess.check_output(['git','-C',str(root),'show',f'{REVISION}:src/spicelib/devices/hisimhv{version}/{base}eval.c'],text=True)
    section=src.split('/* Vdseff (begin) */',1)[1].split('/* Vdseff (end) */',1)[0]
    names=sorted(set(re.findall(r'\b\w+_d(?:V\w+|T)\b',section)))
    derivatives=';'.join(f'double {n}=0.' for n in names)+';'
    return f'''#include <math.h>
#include <float.h>
#include <stdio.h>
#define Fn_Pow powl
#define Fn_SZ(y,x,d,dy) do {{long double a=(x),r=sqrtl(a*a+4.L*(d)*(d)); (y)=(a+r)*.5L; (dy)=(y)/r;}} while(0)
int main(void) {{ long double x[7]; while(1) {{
for(int i=0;i<7;i++) if(scanf("%Lf",&x[i])!=1)return 0;
long double Vds=x[0],Vgp=x[1],Vbsz=x[2],beta_inv=x[3],Cox=x[4],small=1e-25L,epsm10=10.L*DBL_EPSILON;
struct {{long double {prefix}_qnsub_esi,{prefix}_ddlt;}} instance={{x[5],x[6]}},*here=&instance;
long double Vdsorg=0.,Vdseff=0.,T0=0.,T1=0.,T2=0.,T3=0.,T4=0.,T5=0.,T6=0.,T7=0.,T8=0.,T9=0.,T10=0.,TX=0.;
{derivatives}
{section.replace('sqrt(', 'sqrtl(')}
printf("%.20Lg %.20Lg\\n",Vdseff,T10);
}}}}
'''
def main():
    root,out=map(Path,sys.argv[1:]); cases=list(product((.001,.1,1.,10.,100.),(-.5,.1,1.,5.),(-.2,-1.),(.025,.04),(.004,.01),(1.6021918e-19*1.034943e-10*1e23,),(.8,1.,2.,5.)))
    with tempfile.TemporaryDirectory(prefix='hisim-drain-oracle-') as tmp,out.open('w') as f:
        f.write('# family Vds Vgp Vbsz Vt Cox qnsub_esi ddlt; Vdseff T10 and four independent gradients each\n')
        for version in (1,2):
            c,exe=Path(tmp)/'oracle.c',Path(tmp)/'oracle'; c.write_text(program(root,version));subprocess.run(['sccache','cc','-std=c11','-O2',str(c),'-lm','-o',str(exe)],check=True)
            queries=[]
            for case in cases:
                queries.append(case)
                for axis in range(4):
                    for sign in (-1,1):
                        queries.append(tuple(value+sign*1e-6*GRADIENTS[i][axis] if i<5 else value for i,value in enumerate(case)))
            result=subprocess.run([str(exe)],input=''.join(' '.join(map(str,row))+'\n' for row in queries),text=True,capture_output=True,check=True)
            rows=[tuple(map(float,line.split())) for line in result.stdout.splitlines()]; assert len(rows)==len(queries)
            for i,case in enumerate(cases):
                values=rows[i*9:i*9+9]; gradients=[(values[2*a+2][j]-values[2*a+1][j])/2e-6 for j in range(2) for a in range(4)]; result=(*values[0],*gradients);assert all(map(math.isfinite,result)),(case,result);f.write(' '.join(map(str,(version,*case,*result)))+'\n')
if __name__=='__main__':main()
