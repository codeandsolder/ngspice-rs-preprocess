"""Independent source-extracted poly depletion values and gradients."""
from itertools import product
from decimal import Decimal
from pathlib import Path
import math,re,subprocess,sys,tempfile
REVISION='972dbf45db1e47d18bc4fda5d8bb829aff2d0e05'
GRADIENTS=((1,0,0,0),(0,1,0,0),(0,0,.8,-.0002),(1,.02,.01,.001),(0,.03,.81,.0008),(0,.03,.02,.0001))
def program(root,version):
    prefix='HSMHV' if version==1 else 'HSMHV2'; base='hsmhv' if version==1 else 'hsmhv2'
    src=subprocess.check_output(['git','-C',str(root),'show',f'{REVISION}:src/spicelib/devices/hisimhv{version}/{base}eval.c'],text=True)
    macros=[]
    for name in ('Fn_SU','Fn_SZ','Fn_SL','Fn_DclPoly4','Fn_SUPoly4','Fn_ExpLim'):
        start=src.index('#define '+name+'(');end=re.search(r'(?m)^\s*}[^\S\n]*$',src[start:]).end()+start;macros.append(src[start:end].replace('sqrt (','sqrtl ('))
    symmetry=''
    if version==1:
        start=src.index('T1 = here->HSMHV_qnsub_esi * Cox0_inv',src.index('* Factor of modification for symmetry.'));end=src.index('* Quantum effect',start);symmetry=src[start:src.rfind('/*',start,end)]
    start=src.rindex('* Poly-Depletion Effect');start=src.index('*/',start)+2;end=src.index('Vgs_fb =',start);end=src.index(';',end)+1;section=symmetry+src[start:end]
    section=re.sub(r'/\*.*?\*/','',section,flags=re.S)
    names=set(re.findall(r'\b\w+_d(?:V\w+|T)\b',section));decl=';'.join(f'long double {n}=0.' for n in sorted(names))+';'
    inputs={'Vgs','Vds','Vbscl','Vgsz','Vbsz','Vfb','Vthq','Cox0_inv','Pb2c','dVth','dmpacc','FMDVDS','pol_b','pol_dlt','TMF0','TMF1','TMF2','TMF3','TMF4','T9','T6'}
    lhs=set(re.findall(r'\b([A-Za-z_]\w*)\s*(?:=|\*=|/=|\+=|-=)\s*[^=]',section));lhs-=inputs|names
    lhs.update(re.findall(r'Fn_\w+\(\s*(\w+)\s*,',section));lhs-=inputs|names
    maps={'here':{'qnsub_esi':'density','betatnom':'x[10]','flg_pgd':'x[6]!=0','cnstpgd':'x[6]*powl(1.L+1e-6L/x[9],0.7L)'},'model':{'pgd2':'x[7]'},'pParam':{'pgd3':'x[8]'}}
    structs=[]
    for obj,init in maps.items():
        fields=sorted(set(re.findall(r'\b'+obj+r'->'+prefix+r'_(\w+)',section)))
        if fields:structs.append('struct {'+' '.join(f'long double {prefix}_{field};' for field in fields)+'} '+obj+'_value={'+','.join(init[field] for field in fields)+'},*'+obj+'=&'+obj+'_value;')
    return f'''#include <math.h>
#include <stdio.h>
{chr(10).join(macros)}
int main(void){{long double x[11];while(1){{for(int i=0;i<11;i++)if(scanf("%Lf",&x[i])!=1)return 0;
long double Vgs=x[0],Vds=x[1],Vbscl=x[2],Vgsz=x[3],Vbsz=x[4],dVth=x[5],Vfb=-0.7L,qe=1.6021918e-19L,esi=1.034943e-10L,density=qe*esi*1e23L;
long double Pb2c=2.L/38.68283L*logl(1e23L/1.04e16L),Cox0_inv=10e-9L/(8.8541878e-12L*3.9L),Vthq=Pb2c+Vfb+sqrtl(2*density*Pb2c)*Cox0_inv;
long double pol_b=1.L,pol_dlt=.05L,dmpacc=0,FMDVDS=0,TMF0=0,TMF1=0,TMF2=0,TMF3=0,TMF4=0,T9=0,T6=0;
{chr(10).join(structs)}
{decl}
long double {','.join(n+'=0' for n in sorted(lhs))};
{section.replace('sqrt(', 'sqrtl(').replace('sqrt (','sqrtl (')}
printf("%.20Lg %.20Lg %.20Lg %.20Lg %.20Lg\\n",dPpg,Vgp,Vgs_fb,FMDVDS,dmpacc);
}}}}
'''
def main():
    root,out=map(Path,sys.argv[1:]);
    with tempfile.TemporaryDirectory(prefix='hisim-poly-oracle-') as tmp,out.open('w') as f:
        f.write('# family Vgs Vds Vbscl Vgsz Vbsz dVth pgd1 pgd2 pgd3 length betatnom; dPpg Vgp Vgs_fb FMDVDS dmpacc then four gradients each\n')
        for version in (1,2):
            c,exe=Path(tmp)/'oracle.c',Path(tmp)/'oracle';c.write_text(program(root,version));subprocess.run(['sccache','cc','-std=c11','-O2',str(c),'-lm','-o',str(exe)],check=True)
            cases=[]
            for gate,drain,body,pgd1,pgd2,pgd3,length,beta in product((-4.,-1.,0.,1.,5.),(0.,.01,.5,5.),(-1.,0.,.3),(0.,.03,.15),(-1.,.4),(0.,.8) if version==1 else (0.,),(.8e-6,2e-6),(35.,45.)):
                cases.append((gate,drain,body,gate+.02,body+.02,.05,pgd1,pgd2,pgd3,length,beta))
            queries=[]
            for case in cases:
                queries.append(case)
                for axis in range(4):
                    for sign in (-1,1):queries.append(tuple(v+sign*(1e-3 if axis==3 else 1e-5)*GRADIENTS[i][axis] if i<6 else v for i,v in enumerate(case)))
            result=subprocess.run([str(exe)],input=''.join(' '.join(map(str,row))+'\n' for row in queries),text=True,capture_output=True,check=True)
            rows=[tuple(map(Decimal,line.split())) for line in result.stdout.splitlines()];assert len(rows)==len(queries)
            for i,case in enumerate(cases):
                values=rows[i*9:i*9+9];gradients=[(values[2*a+2][j]-values[2*a+1][j])/Decimal('0.002' if a==3 else '0.00002') for j in range(5) for a in range(4)];result=(*values[0],*gradients);assert all(map(math.isfinite,result)),(case,result);f.write(' '.join(map(str,(version,*case,*result)))+'\n')
if __name__=='__main__':main()
