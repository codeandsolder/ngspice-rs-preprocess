"""Extract native HiSIM-HV body clamp and symmetry bias, with independent gradients."""
from itertools import product
from pathlib import Path
import math
import re
import subprocess
import sys
import tempfile
REVISION='972dbf45db1e47d18bc4fda5d8bb829aff2d0e05'
GRADIENTS=((1,0,0,0),(0,1,0,0),(0,0,1,0),(0,0,0,-.0017),(0,0,0,0),(0,0,0,0),(0,0,0,0))
def program(root,version):
    prefix='HSMHV' if version==1 else 'HSMHV2'; base='hsmhv' if version==1 else 'hsmhv2'
    src=subprocess.check_output(['git','-C',str(root),'show',f'{REVISION}:src/spicelib/devices/hisimhv{version}/{base}eval.c'],text=True)
    macros=[]
    for name in ('Fn_SU','Fn_DclPoly4','Fn_SUPoly4m','Fn_SymAdd'):
        start=src.index('#define '+name+'(');end=re.search(r'(?m)^\s*}[^\S\n]*$',src[start:]).end()+start
        macros.append(src[start:end].replace('sqrt (','sqrtl ('))
    start=src.index('Fn_SU( T1 , Pb2');end=src.index('Vbs_bnd_dT = 0.5 * Vbs_max_dT',start);end=src.index('}',end)+1
    limits=src[start:end]
    start=src.index('/* Clamping for Vbs > Vbs_bnd */');end=src.index('/* Factor of modification for symmetry.',start) if '/* Factor of modification for symmetry.' in src[start:] else src.index('* Factor of modification for symmetry.',start)
    section=src[start:src.rfind('/*',start,end)]
    names=set(re.findall(r'\b\w+_d(?:V\w+|T)\b',limits+section));names.discard('Pb2_dT')
    decl=';'.join(f'long double {n}=0.' for n in sorted(names))+';'
    return f'''#include <math.h>
#include <stdio.h>
{chr(10).join(macros)}
int main(void) {{ long double x[7];while(1){{for(int i=0;i<7;i++)if(scanf("%Lf",&x[i])!=1)return 0;
long double Vgs=x[0],Vds=x[1],Vbs=x[2],Pb2=x[3],Pb20=x[4],Pb2c=x[5],Pb2_dT=-.0017L,ps_conv=1e-12L;
struct {{long double {prefix}_vzadd0;}} m={{x[6]}},*model=&m;
long double Vbs_max=.8L,Vbs_bnd=.4L,Vbscl=0,Vzadd=0,Vbsz=0,Vdsz=0,Vgsz=0,TY=0,TMF0=0,TMF1=0,TMF2=0,TMF3=0,TMF4=0;
long double {','.join('T'+str(i)+'=0' for i in range(18))};
{decl}
{limits}
{section}
printf("%.20Lg %.20Lg %.20Lg %.20Lg %.20Lg %.20Lg %.20Lg\\n",Vbscl,Vbscl_dVbs,Vbs_max,Vzadd,Vbsz,Vdsz,Vgsz);
}}}}
'''
def main():
    root,out=map(Path,sys.argv[1:]);cases=list(product((-.5,1.5),(.0001,.01,1.,20.),(-2.,0.,.2,.39,.5,1.,5.),(.45,.8,1.2),(.55,.9),(0.5,.95),(.001,.02)))
    with tempfile.TemporaryDirectory(prefix='hisim-bias-oracle-') as tmp,out.open('w') as f:
        f.write('# family Vgs Vds Vbs Pb2 Pb20 Pb2c vzadd0; seven output values followed by four derivatives each\n')
        for version in (1,2):
            c,exe=Path(tmp)/'oracle.c',Path(tmp)/'oracle';c.write_text(program(root,version));subprocess.run(['sccache','cc','-std=c11','-O2',str(c),'-lm','-o',str(exe)],check=True)
            queries=[]
            for case in cases:
                queries.append(case)
                for axis in range(4):
                    for sign in (-1,1):queries.append(tuple(v+sign*1e-6*GRADIENTS[i][axis] for i,v in enumerate(case)))
            result=subprocess.run([str(exe)],input=''.join(' '.join(map(str,row))+'\n' for row in queries),text=True,capture_output=True,check=True)
            rows=[tuple(map(float,line.split())) for line in result.stdout.splitlines()];assert len(rows)==len(queries)
            for i,case in enumerate(cases):
                values=rows[i*9:i*9+9];gradients=[(values[2*a+2][j]-values[2*a+1][j])/2e-6 for j in range(7) for a in range(4)];result=(*values[0],*gradients);assert all(map(math.isfinite,result)),(case,result);f.write(' '.join(map(str,(version,*case,*result)))+'\n')
if __name__=='__main__':main()
