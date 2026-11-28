"""Independent HiSIM-HV quantum oxide source oracle, values and full gradients."""
from itertools import product
from decimal import Decimal
from pathlib import Path
import math
import re
import subprocess
import sys
import tempfile
REVISION='972dbf45db1e47d18bc4fda5d8bb829aff2d0e05'
GRADIENTS=((1,0,0,0),(0,0,1,0),(1,.2,-.08,.0004),(0,0,0,.000003))
def program(root,version):
    prefix='HSMHV' if version==1 else 'HSMHV2'; base='hsmhv' if version==1 else 'hsmhv2'
    src=subprocess.check_output(['git','-C',str(root),'show',f'{REVISION}:src/spicelib/devices/hisimhv{version}/{base}eval.c'],text=True)
    macros=[]
    for name in ('Fn_SU','Fn_SZ'):
        start=src.index('#define '+name+'(');end=re.search(r'(?m)^\s*}[^\S\n]*$',src[start:]).end()+start;macros.append(src[start:end].replace('sqrt (','sqrtl ('))
    start=src.index('if ( model->'+prefix+'_flg_qme == 0 )');end=src.index('* Vbsz2 : Vbs for dVth',start);section=src[start:src.rfind('/*',start,end)]
    names=set(re.findall(r'\b\w+_d(?:V\w+|T)\b',section));decl=';'.join(f'long double {n}=0.' for n in sorted(names))+';'
    return f'''#include <math.h>
#include <stdio.h>
{chr(10).join(macros)}
int main(void) {{long double x[11];while(1){{for(int i=0;i<11;i++)if(scanf("%Lf",&x[i])!=1)return 0;
long double Vgs=x[0],Vbs=x[1],Vgsz=x[2],cnst0=x[3],Tox0=x[4],c_eox=x[5],Vfb=x[6],Pb20=x[7],Cox0=c_eox/Tox0,Cox0_inv=1.L/Cox0,small=1e-25L,qme_dlt=1e-11L,qme2_dlt=.05L;
long double qme1=x[8],qme2=x[9],qme3=x[10]; int flg_qme=0;
struct {{long double {prefix}_qme1,{prefix}_qme2,{prefix}_qme3,{prefix}_qme12;int {prefix}_flg_qme;}} m={{qme1,qme2,qme3,qme2==0?0:qme1/(qme2*qme2),(qme1!=0||qme3!=0)&&qme2!=0}},*model=&m;
struct {{long double {prefix}_2qnsub_esi;}} i={{2.L*1.6021918e-19L*1.034943e-10L*1e23L}},*here=&i;
long double Vthq=0,Tox=0,Cox=0,Cox_inv=0,cnstCoxi=0,dTox=0,TMF1=0,TMF2=0;
long double {','.join('T'+str(i)+'=0' for i in range(10))};
{decl}
{section.replace('sqrt(', 'sqrtl(').replace('sqrt (','sqrtl (').replace('fabs(', 'fabsl(')}
printf("%.20Lg %.20Lg %.20Lg %.20Lg %d\\n",Tox,Cox,Cox_inv,cnstCoxi,flg_qme);
}}}}
'''
def main():
    root,out=map(Path,sys.argv[1:]);cases=[]
    for gate,body,offset,cnst,oxide,kappa,flatband,coefficients in product((-3.,0.,.5,2.,5.),(-2.,-.3,0.,.5),(.002,.02),(.0001,.001),(5e-9,15e-9),(3.9,),(-.7,.1),((0.,0.,0.),(0.,2.,0.),(1e-10,0.,2e-10),(1e-10,1.,0.),(8e-10,2.,0.),(1e-10,2.,2e-10),(0.,2.,2e-10),(8e-10,2.,2e-10))):
        qme1,qme2,qme3=coefficients
        cases.append((gate,body,gate+offset,cnst,oxide,8.8541878e-12*kappa,flatband,2/38.68283*math.log(1e23/1.04e16),qme1,qme2,qme3))
    with tempfile.TemporaryDirectory(prefix='hisim-quantum-oracle-') as tmp,out.open('w') as f:
        f.write('# family Vgs Vbs Vgsz cnst0 tox permittivity vfbc Pb20 qme1 qme2 qme3; Tox Cox Cox_inv cnstCoxi active then four gradients each\n')
        for version in (1,2):
            c,exe=Path(tmp)/'oracle.c',Path(tmp)/'oracle';c.write_text(program(root,version));subprocess.run(['sccache','cc','-std=c11','-O2',str(c),'-lm','-o',str(exe)],check=True)
            queries=[]
            for case in cases:
                queries.append(case)
                for axis in range(4):
                    for sign in (-1,1):queries.append(tuple(v+sign*(1e-3 if axis==3 else 1e-5)*GRADIENTS[i][axis] if i<4 else v for i,v in enumerate(case)))
            result=subprocess.run([str(exe)],input=''.join(' '.join(map(str,row))+'\n' for row in queries),text=True,capture_output=True,check=True)
            rows=[tuple(map(Decimal,line.split())) for line in result.stdout.splitlines()];assert len(rows)==len(queries)
            for i,case in enumerate(cases):
                values=rows[i*9:i*9+9];gradients=[(values[2*a+2][j]-values[2*a+1][j])/Decimal("0.002" if a==3 else "0.00002") for j in range(4) for a in range(4)];result=(*values[0],*gradients);assert all(map(math.isfinite,result)),(case,result);f.write(' '.join(map(str,(version,*case,*result)))+'\n')
if __name__=='__main__':main()
